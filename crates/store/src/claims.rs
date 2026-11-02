//! Claims as stored (V14, architecture §18.5): what a source asserted
//! about a key, kept in `unverified_items.claims` and `torrent_claims.data`.
//!
//! The model mirrors `lodestar_sources::Claims` and `Provenance` (that
//! crate depends on this one, so it can't be shared), with the same
//! limits. SRC-2 maps one onto the other.
//!
//! **The encoding is an on-disk format and an untrusted-input parser.** A
//! store file can be corrupt or crafted, so [`decode`] trusts nothing:
//!
//! ```text
//! blob   = version:u8 (1) · codec:u8 (0 raw, 1 zstd) · body
//! plain  = present:u8 · confidence:u8
//!          [name:str] [size:uvar] [description:str] [publisher:str]
//!          [external_ref:str] [published_at:svar]
//!          files:uvar  (path:str0 · has_size:u8 · [size:uvar])*
//!          trackers:uvar (str)*
//!          ids:uvar (scheme:str · value:str)*
//! str    = len:uvar (≥ 1) · UTF-8 bytes;  str0 allows len 0 (paths)
//! uvar   = LEB128, at most 10 bytes, minimal (no trailing zero groups)
//! svar   = uvar of the zigzag value
//! ```
//!
//! - `present` has one bit per optional field, in the order above; bits 6
//!   and 7 are zero.
//! - Every length and count is checked against its limit before anything
//!   is allocated, the text total against [`MAX_TOTAL`], the body against
//!   [`MAX_PLAIN`] (zstd is asked for at most that much), and nothing may
//!   follow the last field.
//! - The plain layer is canonical: a body [`decode_plain`] accepts
//!   re-encodes to the same bytes. The fuzz target checks that.
//! - A new layout takes a new version byte; [`decode`] refuses versions it
//!   doesn't know rather than guessing.

use std::fmt;

/// Bytes in a claimed name.
pub const MAX_NAME: usize = 1024;
/// Claimed files per key.
pub const MAX_FILES: usize = 10_000;
/// Bytes in one claimed file path.
pub const MAX_PATH: usize = 4096;
/// Claimed trackers per key.
pub const MAX_TRACKERS: usize = 64;
/// Bytes in one tracker URL.
pub const MAX_TRACKER: usize = 2048;
/// External IDs per key.
pub const MAX_IDS: usize = 32;
/// Bytes in an ID's scheme or value.
pub const MAX_ID_PART: usize = 128;
/// Bytes in a claimed description.
pub const MAX_DESCRIPTION: usize = 64 * 1024;
/// Bytes in a publisher identity.
pub const MAX_PUBLISHER: usize = 256;
/// Bytes in a provenance external reference.
pub const MAX_EXTERNAL_REF: usize = 2048;
/// Bytes of claim text in all (provenance aside), as `lodestar_sources`
/// clamps it.
pub const MAX_TOTAL: usize = 1024 * 1024;
/// The largest plain body: the text, the external reference, and the
/// framing: at most 13 bytes a file (a 2-byte path length, the size flag
/// and a 10-byte size) and under 3 KiB for the rest.
pub const MAX_PLAIN: usize = MAX_TOTAL + MAX_EXTERNAL_REF + MAX_FILES * 13 + 4096;
/// The largest stored blob: a body is stored raw when zstd doesn't make it
/// smaller. The schema's `CHECK` holds the same number.
pub const MAX_STORED: usize = MAX_PLAIN + 2;

/// The only layout so far.
pub const VERSION: u8 = 1;
const RAW: u8 = 0;
const ZSTD: u8 = 1;

const NAME: u8 = 1 << 0;
const SIZE: u8 = 1 << 1;
const DESCRIPTION: u8 = 1 << 2;
const PUBLISHER: u8 = 1 << 3;
const EXTERNAL_REF: u8 = 1 << 4;
const PUBLISHED_AT: u8 = 1 << 5;
const KNOWN: u8 = (1 << 6) - 1;

/// One claimed file. The size is what the source says, not what's true.
#[derive(Clone, PartialEq, Eq)]
pub struct ClaimedFile {
    /// May be empty: the filter pipeline judges it (SRC-0).
    pub path: String,
    pub size: Option<u64>,
}

/// An external ID such as `imdb:tt0111161`. The scheme is lower case.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalId {
    pub scheme: String,
    pub value: String,
}

/// The path's length only, as for [`Claims`].
impl fmt::Debug for ClaimedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimedFile")
            .field("path_len", &self.path.len())
            .field("size", &self.size)
            .finish()
    }
}

/// The scheme, but the value's length only.
impl fmt::Debug for ExternalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalId")
            .field("scheme_len", &self.scheme.len())
            .field("value_len", &self.value.len())
            .finish()
    }
}

/// How much the source's word is worth, as its plugin judges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Confidence {
    #[default]
    Low = 0,
    Medium = 1,
    High = 2,
}

/// Where the claims came from, beyond the source id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Provenance {
    /// A feed item's GUID or page URL.
    pub external_ref: Option<String>,
    /// When the source says it was published, Unix seconds.
    pub published_at: Option<i64>,
    pub confidence: Confidence,
}

/// Everything one source asserted about one key.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Claims {
    pub name: Option<String>,
    pub size: Option<u64>,
    pub files: Vec<ClaimedFile>,
    pub trackers: Vec<String>,
    pub ids: Vec<ExternalId>,
    pub description: Option<String>,
    pub publisher: Option<String>,
    pub provenance: Provenance,
}

/// No text: claims can hold a name a rule holds, and that mustn't reach a
/// log line.
impl fmt::Debug for Claims {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Claims")
            .field("name_len", &self.name.as_ref().map(String::len))
            .field("files", &self.files.len())
            .field("text_len", &self.text_len())
            .finish_non_exhaustive()
    }
}

impl Claims {
    /// Bytes of text held, the measure [`MAX_TOTAL`] bounds.
    pub fn text_len(&self) -> usize {
        let len = |s: &Option<String>| s.as_ref().map_or(0, String::len);
        len(&self.name)
            + len(&self.description)
            + len(&self.publisher)
            + self.trackers.iter().map(String::len).sum::<usize>()
            + self
                .ids
                .iter()
                .map(|i| i.scheme.len() + i.value.len())
                .sum::<usize>()
            + self.files.iter().map(|f| f.path.len()).sum::<usize>()
    }

    /// Whether the claims are within every limit, in the shape
    /// `lodestar_sources::Claims::clamp` leaves them: no empty optional
    /// strings, trackers or ID parts, and lower-case ID schemes.
    pub fn check(&self) -> Result<(), ClaimsError> {
        let opt = |s: &Option<String>, max, what| match s {
            Some(v) if v.is_empty() || v.len() > max => Err(ClaimsError::Limit(what)),
            _ => Ok(()),
        };
        opt(&self.name, MAX_NAME, "name")?;
        opt(&self.description, MAX_DESCRIPTION, "description")?;
        opt(&self.publisher, MAX_PUBLISHER, "publisher")?;
        opt(
            &self.provenance.external_ref,
            MAX_EXTERNAL_REF,
            "external reference",
        )?;
        if self.files.len() > MAX_FILES || self.files.iter().any(|f| f.path.len() > MAX_PATH) {
            return Err(ClaimsError::Limit("files"));
        }
        if self.trackers.len() > MAX_TRACKERS
            || self
                .trackers
                .iter()
                .any(|t| t.is_empty() || t.len() > MAX_TRACKER)
        {
            return Err(ClaimsError::Limit("trackers"));
        }
        let part = |s: &str| !s.is_empty() && s.len() <= MAX_ID_PART;
        if self.ids.len() > MAX_IDS
            || self.ids.iter().any(|i| {
                !part(&i.scheme)
                    || !part(&i.value)
                    || i.scheme.bytes().any(|b| b.is_ascii_uppercase())
            })
        {
            return Err(ClaimsError::Limit("ids"));
        }
        if self.text_len() > MAX_TOTAL {
            return Err(ClaimsError::Limit("total"));
        }
        Ok(())
    }
}

/// Why claims couldn't be encoded or decoded. Carries no claim text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimsError {
    /// A version byte this build doesn't know.
    Version(u8),
    /// A codec byte this build doesn't know.
    Codec(u8),
    /// zstd refused the body, or it decompresses past [`MAX_PLAIN`].
    Compression,
    /// The blob or body ended inside a field.
    Truncated,
    /// Bytes after the last field.
    Trailing,
    /// A value the layout forbids (a non-minimal varint, unknown bits, a
    /// bad flag byte, invalid UTF-8).
    Malformed(&'static str),
    /// A field over its limit, or empty where it can't be.
    Limit(&'static str),
}

impl fmt::Display for ClaimsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClaimsError::Version(v) => write!(f, "unknown claims version {v}"),
            ClaimsError::Codec(c) => write!(f, "unknown claims codec {c}"),
            ClaimsError::Compression => f.write_str("claims don't decompress within bounds"),
            ClaimsError::Truncated => f.write_str("claims end inside a field"),
            ClaimsError::Trailing => f.write_str("bytes after the claims"),
            ClaimsError::Malformed(w) => write!(f, "malformed claims: {w}"),
            ClaimsError::Limit(w) => write!(f, "claims over a limit: {w}"),
        }
    }
}

impl std::error::Error for ClaimsError {}

/// The stored blob for `claims`: checked, laid out, and compressed at
/// zstd `level` when that makes it smaller.
pub fn encode(claims: &Claims, level: i32) -> Result<Vec<u8>, ClaimsError> {
    let plain = encode_plain(claims)?;
    let mut out = Vec::with_capacity(plain.len() + 2);
    out.push(VERSION);
    match zstd::bulk::compress(&plain, level) {
        Ok(z) if z.len() < plain.len() => {
            out.push(ZSTD);
            out.extend_from_slice(&z);
        }
        _ => {
            out.push(RAW);
            out.extend_from_slice(&plain);
        }
    }
    Ok(out)
}

/// Reads a stored blob back. Bounded: it never holds more than
/// [`MAX_PLAIN`] bytes of body.
pub fn decode(blob: &[u8]) -> Result<Claims, ClaimsError> {
    let (&version, rest) = blob.split_first().ok_or(ClaimsError::Truncated)?;
    if version != VERSION {
        return Err(ClaimsError::Version(version));
    }
    let (&codec, body) = rest.split_first().ok_or(ClaimsError::Truncated)?;
    match codec {
        RAW => {
            if body.len() > MAX_PLAIN {
                return Err(ClaimsError::Limit("body"));
            }
            decode_plain(body)
        }
        ZSTD => {
            if body.len() > MAX_PLAIN {
                return Err(ClaimsError::Limit("body"));
            }
            // Reserve what the frame declares, never more than the cap: a
            // larger body fails the decompression instead.
            let declared = zstd::zstd_safe::get_frame_content_size(body)
                .map_err(|_| ClaimsError::Compression)?
                .map_or(MAX_PLAIN, |n| n.min(MAX_PLAIN as u64) as usize);
            let plain =
                zstd::bulk::decompress(body, declared).map_err(|_| ClaimsError::Compression)?;
            decode_plain(&plain)
        }
        c => Err(ClaimsError::Codec(c)),
    }
}

/// The plain body of `claims` (no version or codec byte).
pub fn encode_plain(claims: &Claims) -> Result<Vec<u8>, ClaimsError> {
    claims.check()?;
    let c = claims;
    let p = &c.provenance;
    let mut present = 0;
    for (on, bit) in [
        (c.name.is_some(), NAME),
        (c.size.is_some(), SIZE),
        (c.description.is_some(), DESCRIPTION),
        (c.publisher.is_some(), PUBLISHER),
        (p.external_ref.is_some(), EXTERNAL_REF),
        (p.published_at.is_some(), PUBLISHED_AT),
    ] {
        if on {
            present |= bit;
        }
    }
    let mut out = Vec::with_capacity(64 + c.text_len());
    out.push(present);
    out.push(p.confidence as u8);
    let put_str = |out: &mut Vec<u8>, s: &str| {
        put_uvar(out, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    };
    if let Some(s) = &c.name {
        put_str(&mut out, s);
    }
    if let Some(n) = c.size {
        put_uvar(&mut out, n);
    }
    if let Some(s) = &c.description {
        put_str(&mut out, s);
    }
    if let Some(s) = &c.publisher {
        put_str(&mut out, s);
    }
    if let Some(s) = &p.external_ref {
        put_str(&mut out, s);
    }
    if let Some(t) = p.published_at {
        put_uvar(&mut out, ((t << 1) ^ (t >> 63)) as u64);
    }
    put_uvar(&mut out, c.files.len() as u64);
    for f in &c.files {
        put_str(&mut out, &f.path);
        match f.size {
            Some(n) => {
                out.push(1);
                put_uvar(&mut out, n);
            }
            None => out.push(0),
        }
    }
    put_uvar(&mut out, c.trackers.len() as u64);
    for t in &c.trackers {
        put_str(&mut out, t);
    }
    put_uvar(&mut out, c.ids.len() as u64);
    for i in &c.ids {
        put_str(&mut out, &i.scheme);
        put_str(&mut out, &i.value);
    }
    if out.len() > MAX_PLAIN {
        return Err(ClaimsError::Limit("body"));
    }
    Ok(out)
}

/// Parses a plain body. See the module docs for the layout and checks.
pub fn decode_plain(body: &[u8]) -> Result<Claims, ClaimsError> {
    if body.len() > MAX_PLAIN {
        return Err(ClaimsError::Limit("body"));
    }
    let mut r = Reader {
        rest: body,
        text: 0,
    };
    let present = r.byte()?;
    if present & !KNOWN != 0 {
        return Err(ClaimsError::Malformed("unknown field bits"));
    }
    let confidence = match r.byte()? {
        0 => Confidence::Low,
        1 => Confidence::Medium,
        2 => Confidence::High,
        _ => return Err(ClaimsError::Malformed("confidence")),
    };
    let has = |bit| present & bit != 0;
    let name = has(NAME).then(|| r.text(1, MAX_NAME, "name")).transpose()?;
    let size = has(SIZE).then(|| r.uvar()).transpose()?;
    let description = has(DESCRIPTION)
        .then(|| r.text(1, MAX_DESCRIPTION, "description"))
        .transpose()?;
    let publisher = has(PUBLISHER)
        .then(|| r.text(1, MAX_PUBLISHER, "publisher"))
        .transpose()?;
    // Provenance isn't text the total counts (as in `lodestar_sources`).
    let before = r.text;
    let external_ref = has(EXTERNAL_REF)
        .then(|| r.text(1, MAX_EXTERNAL_REF, "external reference"))
        .transpose()?;
    r.text = before;
    let published_at = has(PUBLISHED_AT)
        .then(|| r.uvar().map(|z| ((z >> 1) as i64) ^ -((z & 1) as i64)))
        .transpose()?;

    // Each file takes at least two bytes (an empty path and its flag), so
    // a count past what's left fails before anything is reserved.
    let n = r.count(MAX_FILES, 2, "files")?;
    let mut files = Vec::with_capacity(n);
    for _ in 0..n {
        let path = r.text(0, MAX_PATH, "file path")?;
        let size = match r.byte()? {
            0 => None,
            1 => Some(r.uvar()?),
            _ => return Err(ClaimsError::Malformed("file size flag")),
        };
        files.push(ClaimedFile { path, size });
    }
    let n = r.count(MAX_TRACKERS, 2, "trackers")?;
    let mut trackers = Vec::with_capacity(n);
    for _ in 0..n {
        trackers.push(r.text(1, MAX_TRACKER, "tracker")?);
    }
    let n = r.count(MAX_IDS, 4, "ids")?;
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        let scheme = r.text(1, MAX_ID_PART, "id scheme")?;
        if scheme.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(ClaimsError::Malformed("id scheme case"));
        }
        let value = r.text(1, MAX_ID_PART, "id value")?;
        ids.push(ExternalId { scheme, value });
    }
    if !r.rest.is_empty() {
        return Err(ClaimsError::Trailing);
    }
    Ok(Claims {
        name,
        size,
        files,
        trackers,
        ids,
        description,
        publisher,
        provenance: Provenance {
            external_ref,
            published_at,
            confidence,
        },
    })
}

fn put_uvar(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Reader<'a> {
    rest: &'a [u8],
    /// Text bytes read so far, against [`MAX_TOTAL`].
    text: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, ClaimsError> {
        let (&b, rest) = self.rest.split_first().ok_or(ClaimsError::Truncated)?;
        self.rest = rest;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ClaimsError> {
        if n > self.rest.len() {
            return Err(ClaimsError::Truncated);
        }
        let (head, rest) = self.rest.split_at(n);
        self.rest = rest;
        Ok(head)
    }

    /// LEB128, minimal, at most 64 bits.
    fn uvar(&mut self) -> Result<u64, ClaimsError> {
        let mut v: u64 = 0;
        for i in 0..10 {
            let b = self.byte()?;
            let low = u64::from(b & 0x7f);
            if i == 9 && b > 1 {
                return Err(ClaimsError::Malformed("varint overflow"));
            }
            v |= low << (7 * i);
            if b & 0x80 == 0 {
                if i > 0 && b == 0 {
                    return Err(ClaimsError::Malformed("varint not minimal"));
                }
                return Ok(v);
            }
        }
        Err(ClaimsError::Malformed("varint too long"))
    }

    /// A count of at most `max` entries of at least `min_each` bytes.
    fn count(
        &mut self,
        max: usize,
        min_each: usize,
        what: &'static str,
    ) -> Result<usize, ClaimsError> {
        let n = self.uvar()?;
        if n > max as u64 {
            return Err(ClaimsError::Limit(what));
        }
        let n = n as usize;
        if n.saturating_mul(min_each) > self.rest.len() {
            return Err(ClaimsError::Truncated);
        }
        Ok(n)
    }

    fn text(&mut self, min: usize, max: usize, what: &'static str) -> Result<String, ClaimsError> {
        let n = self.uvar()?;
        if n < min as u64 || n > max as u64 {
            return Err(ClaimsError::Limit(what));
        }
        let n = n as usize;
        self.text += n;
        if self.text > MAX_TOTAL {
            return Err(ClaimsError::Limit("total"));
        }
        let bytes = self.take(n)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| ClaimsError::Malformed("utf-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Claims {
        Claims {
            name: Some("Some.Show.S01E01.1080p".into()),
            size: Some(1_234_567_890),
            files: vec![
                ClaimedFile {
                    path: "Some.Show.S01E01.mkv".into(),
                    size: Some(1_234_000_000),
                },
                ClaimedFile {
                    path: String::new(),
                    size: None,
                },
            ],
            trackers: vec!["udp://tracker.example:1337/announce".into()],
            ids: vec![ExternalId {
                scheme: "imdb".into(),
                value: "tt0111161".into(),
            }],
            description: Some("é".repeat(100)),
            publisher: Some("uploader".into()),
            provenance: Provenance {
                external_ref: Some("https://feed.example/item/1".into()),
                published_at: Some(-5),
                confidence: Confidence::Medium,
            },
        }
    }

    #[test]
    fn round_trips_raw_and_compressed() {
        for c in [sample(), Claims::default()] {
            let blob = encode(&c, 3).unwrap();
            assert_eq!(blob[0], VERSION);
            assert_eq!(decode(&blob).unwrap(), c);
        }
        let big = Claims {
            description: Some("abc ".repeat(10_000)),
            ..Claims::default()
        };
        let blob = encode(&big, 3).unwrap();
        assert_eq!(blob[1], ZSTD, "repetitive text compresses");
        assert!(blob.len() < 1000);
        assert_eq!(decode(&blob).unwrap(), big);
    }

    #[test]
    fn the_empty_body_is_the_smallest() {
        assert_eq!(
            encode_plain(&Claims::default()).unwrap(),
            vec![0, 0, 0, 0, 0]
        );
        assert_eq!(
            encode(&Claims::default(), 3).unwrap(),
            vec![1, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn refuses_what_it_does_not_know() {
        assert_eq!(decode(&[]), Err(ClaimsError::Truncated));
        assert_eq!(decode(&[2, 0]), Err(ClaimsError::Version(2)));
        assert_eq!(decode(&[1]), Err(ClaimsError::Truncated));
        assert_eq!(decode(&[1, 9, 0]), Err(ClaimsError::Codec(9)));
        assert_eq!(decode(&[1, 1, 0, 1, 2]), Err(ClaimsError::Compression));
        assert_eq!(
            decode_plain(&[0x40, 0, 0, 0, 0]),
            Err(ClaimsError::Malformed("unknown field bits"))
        );
        assert_eq!(
            decode_plain(&[0, 3, 0, 0, 0]),
            Err(ClaimsError::Malformed("confidence"))
        );
        assert_eq!(
            decode_plain(&[0, 0, 0, 0, 0, 0]),
            Err(ClaimsError::Trailing)
        );
        // An empty name, a non-minimal count, a huge count, bad UTF-8.
        assert_eq!(
            decode_plain(&[NAME, 0, 0, 0, 0, 0]),
            Err(ClaimsError::Limit("name"))
        );
        assert_eq!(
            decode_plain(&[0, 0, 0x80, 0, 0, 0]),
            Err(ClaimsError::Malformed("varint not minimal"))
        );
        assert_eq!(
            decode_plain(&[0, 0, 0xff, 0xff, 0xff, 0x7f, 0, 0]),
            Err(ClaimsError::Limit("files"))
        );
        assert_eq!(
            decode_plain(&[0, 0, 100, 0, 0]),
            Err(ClaimsError::Truncated)
        );
        assert_eq!(
            decode_plain(&[NAME, 0, 1, 0xff, 0, 0, 0]),
            Err(ClaimsError::Malformed("utf-8"))
        );
        assert_eq!(
            decode_plain(&[
                SIZE, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02
            ]),
            Err(ClaimsError::Malformed("varint overflow"))
        );
    }

    #[test]
    fn u64_and_i64_extremes_round_trip() {
        for (size, at) in [(u64::MAX, i64::MIN), (0, i64::MAX), (127, 0), (128, -1)] {
            let c = Claims {
                size: Some(size),
                files: vec![ClaimedFile {
                    path: "f".into(),
                    size: Some(size),
                }],
                provenance: Provenance {
                    published_at: Some(at),
                    ..Provenance::default()
                },
                ..Claims::default()
            };
            assert_eq!(decode(&encode(&c, 3).unwrap()).unwrap(), c);
        }
    }

    #[test]
    fn encode_refuses_unclamped_claims() {
        let cases = [
            Claims {
                name: Some(String::new()),
                ..Claims::default()
            },
            Claims {
                name: Some("x".repeat(MAX_NAME + 1)),
                ..Claims::default()
            },
            Claims {
                ids: vec![ExternalId {
                    scheme: "IMDB".into(),
                    value: "1".into(),
                }],
                ..Claims::default()
            },
            Claims {
                trackers: vec![String::new()],
                ..Claims::default()
            },
            Claims {
                files: vec![
                    ClaimedFile {
                        path: "p".repeat(MAX_PATH),
                        size: None
                    };
                    MAX_TOTAL / MAX_PATH + 1
                ],
                ..Claims::default()
            },
        ];
        for c in cases {
            assert!(encode(&c, 3).is_err(), "{c:?}");
        }
    }

    #[test]
    fn the_largest_claims_fit_the_bounds() {
        // Every field at its limit, then files up to the text total.
        let mut c = Claims {
            name: Some("n".repeat(MAX_NAME)),
            size: Some(u64::MAX),
            files: Vec::new(),
            trackers: vec!["t".repeat(MAX_TRACKER); MAX_TRACKERS],
            ids: vec![
                ExternalId {
                    scheme: "s".repeat(MAX_ID_PART),
                    value: "v".repeat(MAX_ID_PART)
                };
                MAX_IDS
            ],
            description: Some("d".repeat(MAX_DESCRIPTION)),
            publisher: Some("p".repeat(MAX_PUBLISHER)),
            provenance: Provenance {
                external_ref: Some("e".repeat(MAX_EXTERNAL_REF)),
                published_at: Some(i64::MIN),
                confidence: Confidence::High,
            },
        };
        let room = MAX_TOTAL - c.text_len();
        let short = room / MAX_FILES;
        c.files = vec![
            ClaimedFile {
                path: "f".repeat(short),
                size: Some(u64::MAX)
            };
            MAX_FILES
        ];
        let plain = encode_plain(&c).unwrap();
        assert!(plain.len() <= MAX_PLAIN, "{} > {MAX_PLAIN}", plain.len());
        assert_eq!(decode_plain(&plain).unwrap(), c);
        let blob = encode(&c, 3).unwrap();
        assert!(blob.len() <= MAX_STORED);
        assert_eq!(decode(&blob).unwrap(), c);
    }

    /// Long paths need a 2-byte length: the worst framing per file still
    /// fits the bound, raw and compressed.
    #[test]
    fn the_worst_framing_fits_the_bounds() {
        for path in [
            "p".repeat(128),
            (0..128u8).map(|i| (b'!' + i % 90) as char).collect(),
        ] {
            let mut c = Claims {
                size: Some(u64::MAX),
                provenance: Provenance {
                    external_ref: Some("e".repeat(MAX_EXTERNAL_REF)),
                    published_at: Some(i64::MIN),
                    confidence: Confidence::High,
                },
                ..Claims::default()
            };
            let long = MAX_TOTAL / 128;
            c.files = (0..MAX_FILES)
                .map(|i| ClaimedFile {
                    path: if i < long {
                        path.clone()
                    } else {
                        String::new()
                    },
                    size: Some(u64::MAX),
                })
                .collect();
            assert!(c.check().is_ok());
            let plain = encode_plain(&c).unwrap();
            assert!(plain.len() <= MAX_PLAIN, "{} > {MAX_PLAIN}", plain.len());
            let blob = encode(&c, 3).unwrap();
            assert!(blob.len() <= MAX_STORED);
            assert_eq!(decode(&blob).unwrap(), c);
        }
    }

    #[test]
    fn debug_shows_no_text() {
        let c = sample();
        let s = format!("{c:?} {:?} {:?}", c.files, c.ids);
        assert!(!s.contains("Some.Show"), "{s}");
        assert!(!s.contains("tt0111161"), "{s}");
        assert!(!s.contains("uploader"), "{s}");
    }
}
