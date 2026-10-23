//! The documents: the unsigned envelope, and the signed manifest, key
//! statement and freshness bodies, with their caps (README.md has the
//! contract). Bodies are parsed only after their signature verifies
//! (`verify`); this module decodes and parses strictly.

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use semver::Version;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::minisign::Signature;
use crate::{Channel, Error, MAX_LIFETIME, Role};

/// The envelope (one per channel, unsigned): 512 KiB.
pub const MAX_ENVELOPE: usize = 512 * 1024;
/// A manifest body: 384 KiB.
pub const MAX_MANIFEST: usize = 384 * 1024;
/// A freshness body: 64 KiB.
pub const MAX_FRESHNESS: usize = 64 * 1024;
/// A key statement body: 2 KiB.
pub const MAX_STATEMENT: usize = 2 * 1024;
/// Statements a document may carry.
pub const MAX_STATEMENTS: usize = 64;
/// Releases in a manifest.
pub const MAX_RELEASES: usize = 64;
/// Artifacts in a release.
pub const MAX_ARTIFACTS: usize = 8;
/// A release's notes: 16 KiB.
pub const MAX_NOTES: usize = 16 * 1024;
/// A version string.
pub const MAX_VERSION: usize = 64;
/// A URL.
pub const MAX_URL: usize = 512;
/// An artifact's size: 256 MiB.
pub const MAX_ARTIFACT_SIZE: u64 = 256 * 1024 * 1024;
/// The only manifest format this build reads.
pub const FORMAT: u64 = 1;

/// Hosts an artifact URL (and every redirect on the way, PR 3) may name.
pub const DOWNLOAD_HOSTS: [&str; 3] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// A signed document: the exact signed bytes (base64) and the minisign
/// signature file's text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signed {
    pub body: String,
    pub minisig: String,
}

/// A key statement as carried in a document: `minisig` is the recovery
/// key's; `cosig`, the old key of that role's, is optional and never
/// sufficient.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedStatement {
    pub body: String,
    pub minisig: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cosig: Option<String>,
}

/// The envelope a check fetches.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub manifest: Signed,
    #[serde(default)]
    pub freshness: Option<Signed>,
}

impl Envelope {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_ENVELOPE {
            return Err(Error::TooLarge);
        }
        strict(bytes)
    }
}

macro_rules! doc_type {
    ($name:ident, $tag:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            #[serde(rename = $tag)]
            Tag,
        }
    };
}
doc_type!(ManifestType, "lodestar-manifest");
doc_type!(StatementType, "lodestar-key");
doc_type!(FreshnessType, "lodestar-freshness");

/// A manifest body.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestBody {
    #[serde(rename = "type")]
    pub kind: ManifestType,
    pub format: u64,
    pub channel: Channel,
    pub timestamp: u64,
    pub expires: u64,
    pub statements: Vec<SignedStatement>,
    pub releases: Vec<Release>,
}

/// One release in a manifest.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    #[serde(deserialize_with = "de_version")]
    pub version: Version,
    /// Unix seconds; the wait after a release counts from here.
    pub date: u64,
    pub level: Level,
    pub security: bool,
    pub manual_steps: bool,
    /// The oldest version this release upgrades from (null: any).
    #[serde(default, deserialize_with = "de_opt_version")]
    pub min_from: Option<Version>,
    pub withdrawn: bool,
    pub migrates: bool,
    pub reindex: bool,
    pub reindex_bytes: u64,
    /// Signed text; the UI renders it as plain text or sanitised Markdown.
    pub notes: String,
    #[serde(default)]
    pub notes_url: Option<String>,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Fix,
    Feature,
    Major,
}

/// A downloadable file, pinned by SHA-256 and size.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub target: Target,
    pub kind: ArtifactKind,
    pub url: String,
    pub sha256: Sha256,
    pub size: u64,
}

/// The shipped targets. A new one needs a new manifest format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Target {
    #[serde(rename = "x86_64-unknown-linux-musl")]
    X86_64LinuxMusl,
    #[serde(rename = "aarch64-unknown-linux-musl")]
    Aarch64LinuxMusl,
    #[serde(rename = "x86_64-pc-windows-msvc")]
    X86_64WindowsMsvc,
}

impl Target {
    /// The target for a build's `LODESTAR_BUILD_TARGET`, if it's shipped.
    pub fn from_triple(triple: &str) -> Option<Self> {
        match triple {
            "x86_64-unknown-linux-musl" => Some(Target::X86_64LinuxMusl),
            "aarch64-unknown-linux-musl" => Some(Target::Aarch64LinuxMusl),
            "x86_64-pc-windows-msvc" => Some(Target::X86_64WindowsMsvc),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    Binary,
    Msi,
    Portable,
}

/// A key statement body.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatementBody {
    #[serde(rename = "type")]
    pub kind: StatementType,
    pub seq: u64,
    pub role: Role,
    /// The new key, as a minisign public-key base64 line.
    pub key: String,
    pub timestamp: u64,
}

/// A freshness body: keeps an already-signed manifest current.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreshnessBody {
    #[serde(rename = "type")]
    pub kind: FreshnessType,
    pub channel: Channel,
    pub manifest_sha256: Sha256,
    pub timestamp: u64,
    pub expires: u64,
    pub statements: Vec<SignedStatement>,
}

/// A SHA-256 digest, written as 64 lower-case hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256(pub [u8; 32]);

impl Sha256 {
    pub fn of(data: &[u8]) -> Self {
        use sha2::Digest as _;
        Sha256(sha2::Sha256::digest(data).into())
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() != 64 {
            return None;
        }
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut out = [0; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = (digit(b[2 * i])? << 4) | digit(b[2 * i + 1])?;
        }
        Some(Sha256(out))
    }
}

impl std::fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sha256({})", self.to_hex())
    }
}

impl Serialize for Sha256 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Sha256::from_hex(&s).ok_or_else(|| D::Error::custom("sha256 must be 64 lower-case hex"))
    }
}

fn parse_version(s: &str) -> Option<Version> {
    if s.len() > MAX_VERSION {
        return None;
    }
    Version::parse(s).ok()
}

fn de_version<'de, D: Deserializer<'de>>(d: D) -> Result<Version, D::Error> {
    let s = String::deserialize(d)?;
    parse_version(&s).ok_or_else(|| D::Error::custom("not a strict semver version"))
}

fn de_opt_version<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Version>, D::Error> {
    match Option::<String>::deserialize(d)? {
        None => Ok(None),
        Some(s) => parse_version(&s)
            .map(Some)
            .ok_or_else(|| D::Error::custom("not a strict semver version")),
    }
}

/// Decodes a signed document's body (strict base64, at most `cap` bytes)
/// and parses its signature file. Nothing in the body is read.
pub(crate) fn decode(body: &str, minisig: &str, cap: usize) -> Result<(Vec<u8>, Signature), Error> {
    if body.len() > cap.div_ceil(3) * 4 {
        return Err(Error::TooLarge);
    }
    let bytes = STANDARD.decode(body).map_err(|_| Error::BadEncoding)?;
    if bytes.len() > cap {
        return Err(Error::TooLarge);
    }
    Ok((bytes, Signature::parse(minisig)?))
}

/// Strict JSON: UTF-8 without a BOM, the shape's own rules (unknown and
/// duplicate fields refused, integers only).
pub(crate) fn strict<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::BadDocument)?;
    if text.starts_with('\u{feff}') {
        return Err(Error::BadDocument);
    }
    serde_json::from_str(text).map_err(|_| Error::BadDocument)
}

/// The `type` and `format` of a verified body, read leniently so a newer
/// format is reported as such rather than as unknown fields.
pub(crate) fn head(bytes: &[u8], expect: &str) -> Result<Option<u64>, Error> {
    #[derive(Deserialize)]
    struct Head {
        #[serde(rename = "type")]
        kind: Option<String>,
        format: Option<serde_json::Value>,
    }
    let head: Head = strict(bytes)?;
    if head.kind.as_deref() != Some(expect) {
        return Err(Error::WrongType);
    }
    Ok(head.format.and_then(|f| f.as_u64()))
}

/// The `statements` array of a body whose signer isn't known yet. Each
/// entry is a separately signed document, verified before its own body
/// is read; nothing else here is used.
pub(crate) fn statements_ahead(bytes: &[u8]) -> Result<Vec<SignedStatement>, Error> {
    #[derive(Deserialize)]
    struct Ahead {
        #[serde(default)]
        statements: Vec<SignedStatement>,
    }
    let ahead: Ahead = strict(bytes)?;
    if ahead.statements.len() > MAX_STATEMENTS {
        return Err(Error::BadDocument);
    }
    Ok(ahead.statements)
}

impl ManifestBody {
    /// Parses a verified manifest body and checks every cap.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_MANIFEST {
            return Err(Error::TooLarge);
        }
        match head(bytes, "lodestar-manifest")? {
            Some(FORMAT) => {}
            Some(f) if f > FORMAT => return Err(Error::NewerFormat),
            _ => return Err(Error::BadDocument),
        }
        let body: ManifestBody = strict(bytes)?;
        body.validate()?;
        Ok(body)
    }

    fn validate(&self) -> Result<(), Error> {
        expiry_ok(self.timestamp, self.expires)?;
        if self.statements.len() > MAX_STATEMENTS || self.releases.len() > MAX_RELEASES {
            return Err(Error::BadDocument);
        }
        let mut versions = HashSet::new();
        for r in &self.releases {
            // Build metadata doesn't order, so it can't tell two releases apart.
            if !versions.insert((
                r.version.major,
                r.version.minor,
                r.version.patch,
                r.version.pre.clone(),
            )) {
                return Err(Error::BadDocument);
            }
            if r.notes.len() > MAX_NOTES || r.artifacts.len() > MAX_ARTIFACTS {
                return Err(Error::BadDocument);
            }
            if let Some(u) = &r.notes_url {
                url_ok(u, None)?;
            }
            let mut kinds = HashSet::new();
            for a in &r.artifacts {
                if !kinds.insert((a.target, a.kind)) || a.size == 0 || a.size > MAX_ARTIFACT_SIZE {
                    return Err(Error::BadDocument);
                }
                url_ok(&a.url, Some(&DOWNLOAD_HOSTS))?;
            }
        }
        Ok(())
    }
}

impl StatementBody {
    /// Parses a verified key statement body.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_STATEMENT {
            return Err(Error::TooLarge);
        }
        head(bytes, "lodestar-key")?;
        let body: StatementBody = strict(bytes)?;
        if body.seq == 0 {
            return Err(Error::BadDocument);
        }
        Ok(body)
    }
}

impl FreshnessBody {
    /// Parses a verified freshness body.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_FRESHNESS {
            return Err(Error::TooLarge);
        }
        head(bytes, "lodestar-freshness")?;
        let body: FreshnessBody = strict(bytes)?;
        expiry_ok(body.timestamp, body.expires)?;
        if body.statements.len() > MAX_STATEMENTS {
            return Err(Error::BadDocument);
        }
        Ok(body)
    }
}

/// `expires` is after `timestamp` and at most 30 days after it.
pub(crate) fn expiry_ok(timestamp: u64, expires: u64) -> Result<(), Error> {
    if expires <= timestamp || expires - timestamp > MAX_LIFETIME {
        return Err(Error::BadExpiry);
    }
    Ok(())
}

/// An `https` URL of at most 512 bytes: a lower-case host (on `hosts`
/// when given), no user, port, query or fragment, and a path of URL-safe
/// characters.
pub fn url_ok(url: &str, hosts: Option<&[&str]>) -> Result<(), Error> {
    let rest = url.strip_prefix("https://").ok_or(Error::BadDocument)?;
    if url.len() > MAX_URL {
        return Err(Error::BadDocument);
    }
    let (host, path) = rest.split_at(rest.find('/').ok_or(Error::BadDocument)?);
    let host_ok = !host.is_empty()
        && host.split('.').all(|l| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        });
    let path_ok = path
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"/-._~%!$&'()*+,;=:@".contains(&c));
    if !host_ok || !path_ok || hosts.is_some_and(|h| !h.contains(&host)) {
        return Err(Error::BadDocument);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let ok = "https://github.com/dvortsis/lodestar-v2/releases/download/v0.5.3/lodestar";
        assert!(url_ok(ok, Some(&DOWNLOAD_HOSTS)).is_ok());
        for bad in [
            "http://github.com/a",
            "https://evil.example/a",
            "https://github.com.evil.example/a",
            "https://user@github.com/a",
            "https://github.com:443/a",
            "https://github.com/a?x=1",
            "https://github.com/a#x",
            "https://GitHub.com/a",
            "https://github.com",
            "https://github.com/a b",
            "https://github.com/a\\b",
        ] {
            assert!(url_ok(bad, Some(&DOWNLOAD_HOSTS)).is_err(), "{bad}");
        }
        assert!(url_ok(&format!("https://github.com/{}", "a".repeat(500)), None).is_err());
        assert!(url_ok("https://docs.example/notes/0.5.3", None).is_ok());
    }

    #[test]
    fn sha256_hex_is_strict() {
        let s = Sha256::of(b"x");
        assert_eq!(Sha256::from_hex(&s.to_hex()), Some(s));
        assert!(Sha256::from_hex(&s.to_hex().to_uppercase()).is_none());
        assert!(Sha256::from_hex(&s.to_hex()[1..]).is_none());
    }

    #[test]
    fn strict_json_refuses_bom_duplicates_unknowns_and_floats() {
        let key = r#"{"type":"lodestar-key","seq":1,"role":"release","key":"k","timestamp":5}"#;
        assert!(StatementBody::parse(key.as_bytes()).is_ok());
        for bad in [
            format!("\u{feff}{key}"),
            key.replace(r#""seq":1"#, r#""seq":1,"seq":2"#),
            key.replace(r#""seq":1"#, r#""seq":1,"extra":0"#),
            key.replace(r#""seq":1"#, r#""seq":1.0"#),
            key.replace(r#""seq":1"#, r#""seq":1e0"#),
            key.replace(r#""seq":1"#, r#""seq":0"#),
            key.replace(r#""seq":1"#, r#""seq":-1"#),
            key.replace("lodestar-key", "lodestar-manifest"),
            key.replace(r#""role":"release""#, r#""role":"recovery""#),
        ] {
            assert!(StatementBody::parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert_eq!(
            StatementBody::parse(key.replace("lodestar-key", "lodestar-freshness").as_bytes()),
            Err(Error::WrongType)
        );
        assert!(StatementBody::parse(&[0xff, b'{', b'}']).is_err());
    }

    #[test]
    fn decode_caps_before_decoding() {
        let big = STANDARD.encode(vec![0u8; MAX_STATEMENT + 1]);
        assert_eq!(
            decode(&big, "", MAX_STATEMENT).unwrap_err(),
            Error::TooLarge
        );
        assert_eq!(
            decode("a b=", "", MAX_STATEMENT).unwrap_err(),
            Error::BadEncoding
        );
    }
}
