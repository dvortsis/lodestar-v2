//! Building blocks the feed parsers share: the caps, the summary types,
//! hash and magnet parsing, title cleaning, credential stripping and
//! RFC 2822 dates. Moved here from `torznab` for SRC-8.

use lodestar_metainfo::{InfoHash, InfoHashV2};
use quick_xml::XmlVersion;
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesRef, BytesStart};

/// Bytes in a kept title: the claims' name limit.
pub const MAX_TITLE: usize = 1024;
/// Bytes in a kept GUID or link: the claims' external-reference limit.
/// A longer one is dropped, since a cut URL points somewhere else.
pub const MAX_REF: usize = 2048;

/// Caps on one parse. The defaults are [`LIMITS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open elements at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Elements in the document ([`Stop::Elements`]).
    pub max_elements: usize,
    /// `<item>`s in the document, kept or skipped ([`Stop::Items`]).
    pub max_items: usize,
    /// Attributes on an element the parser reads.
    pub max_attrs: usize,
    /// Bytes in an attribute value the parser reads.
    pub max_attr_len: usize,
    /// Bytes of text in a field the parser reads.
    pub max_text: usize,
}

/// The caps for a real feed. Why each value: `docs/decisions/2026-11-01-SRC-6.md`.
pub const LIMITS: Limits = Limits {
    max_bytes: 16 * 1024 * 1024,
    max_depth: 16,
    max_elements: 1_000_000,
    max_items: 5_000,
    max_attrs: 16,
    max_attr_len: 16 * 1024,
    max_text: 64 * 1024,
};

/// Both forms of a torrent's identity; at least one is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hashes {
    pub v1: Option<InfoHash>,
    pub v2: Option<InfoHashV2>,
}

impl Hashes {
    /// The DHT key: the v1 hash, or the v2 hash truncated (BEP 52).
    pub fn key(&self) -> Option<InfoHash> {
        self.v1.or(self.v2.map(|h| h.truncated()))
    }
}

/// Items skipped, by reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// No info hash and no magnet (and not a Newznab item).
    pub no_hash: u32,
    /// An info hash or magnet that isn't a v1 or v2 hash.
    pub bad_hash: u32,
    /// A field over a cap in [`Limits`].
    pub over_cap: u32,
    /// An unknown entity or a malformed attribute.
    pub malformed: u32,
}

impl Skipped {
    pub fn total(&self) -> u32 {
        self.no_hash + self.bad_hash + self.over_cap + self.malformed
    }
}

/// Why a parse ended before the end of a well-formed document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Bytes,
    /// The body isn't UTF-8 past some point; the prefix was parsed.
    NotUtf8,
    Doctype,
    Depth,
    Elements,
    Items,
    /// Not well-formed XML, including a document cut short.
    Malformed,
    /// More namespace declarations in scope than the RSS parser tracks.
    Namespaces,
}

/// How a parse went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub kept: u32,
    pub skipped: Skipped,
    pub stop: Option<Stop>,
    /// The code of a Torznab `<error code="…">` answer. Its description
    /// is the server's text and isn't kept.
    pub error: Option<u32>,
}

/// The body as text: cut to `max_bytes`, then to its valid UTF-8 prefix,
/// without a byte-order mark. A cut is recorded in `out.stop`.
pub(crate) fn prepare<'b>(body: &'b [u8], limits: &Limits, out: &mut Summary) -> &'b str {
    let mut body = body;
    if body.len() > limits.max_bytes {
        body = &body[..limits.max_bytes];
        out.stop = Some(Stop::Bytes);
    }
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        Err(e) => {
            out.stop.get_or_insert(Stop::NotUtf8);
            std::str::from_utf8(&body[..e.valid_up_to()]).unwrap_or_default()
        }
    };
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// A character reference or one of the five XML entities; anything else
/// (an entity a DTD would define, or a bad reference) is `None`.
pub(crate) fn resolve_ref(r: &BytesRef<'_>) -> Option<String> {
    match r.resolve_char_ref() {
        Ok(Some(c)) => Some(c.encode_utf8(&mut [0; 4]).to_owned()),
        Ok(None) => resolve_xml_entity(r).map(str::to_owned),
        Err(_) => None,
    }
}

/// Why an item is skipped, when a field breaks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bad {
    OverCap,
    Malformed,
}

/// Reads `e`'s attributes within the caps, handing each to `f`.
pub(crate) fn attrs(
    e: &BytesStart<'_>,
    limits: &Limits,
    mut f: impl FnMut(&str, &str),
) -> Result<(), Bad> {
    for (i, a) in e.attributes().enumerate() {
        if i >= limits.max_attrs {
            return Err(Bad::OverCap);
        }
        let a = a.map_err(|_| Bad::Malformed)?;
        if a.value.len() > limits.max_attr_len {
            return Err(Bad::OverCap);
        }
        let v = a
            .normalized_value_with(XmlVersion::Implicit1_0, 1, resolve_xml_entity)
            .map_err(|_| Bad::Malformed)?;
        f(a.key.as_ref(), &v);
    }
    Ok(())
}

/// The attribute's hash, with the magnet's other version when the two
/// agree on the version they share: Prowlarr sends `infohash` (v1) and a
/// hybrid's v2 only in the magnet.
pub(crate) fn merge(mut h: Hashes, magnet: Option<Hashes>) -> Hashes {
    let Some(m) = magnet else { return h };
    let v1 = h.v1.is_none() || m.v1.is_none() || h.v1 == m.v1;
    let v2 = h.v2.is_none() || m.v2.is_none() || h.v2 == m.v2;
    if v1 && v2 {
        h.v1 = h.v1.or(m.v1);
        h.v2 = h.v2.or(m.v2);
    }
    h
}

/// A title with control characters removed, cut to [`MAX_TITLE`] bytes
/// on a character boundary.
pub(crate) fn clean_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_TITLE));
    for c in s.chars().filter(|&c| !unsafe_char(c)) {
        if out.len() + c.len_utf8() > MAX_TITLE {
            break;
        }
        out.push(c);
    }
    out
}

/// Controls, and the bidi and format characters that reorder or hide
/// text (the set `crates/api/src/digest.rs` strips).
pub(crate) fn unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
        )
}

pub(crate) fn is_magnet(s: &str) -> bool {
    s.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("magnet:"))
}

/// An `infohash` value: 40 hex digits or 32 base32 characters (v1), or
/// 64 hex digits (v2).
pub(crate) fn hash(s: &str) -> Option<Hashes> {
    let (v1, v2) = match s.len() {
        40 => (Some(s.parse().ok()?), None),
        32 => (Some(InfoHash(base32(s)?)), None),
        64 => (None, Some(s.parse().ok()?)),
        _ => return None,
    };
    let h = Hashes { v1, v2 };
    (!is_zero(&h)).then_some(h)
}

/// The hashes in a magnet's `xt` parameters: `urn:btih:` (v1, hex or
/// base32) and `urn:btmh:1220…` (v2, a SHA-256 multihash).
pub(crate) fn magnet_hashes(m: &str) -> Option<Hashes> {
    let query = m.split_once('?')?.1;
    let mut h = Hashes { v1: None, v2: None };
    for part in query.split('&') {
        let Some(xt) = part
            .get(..3)
            .filter(|p| p.eq_ignore_ascii_case("xt="))
            .map(|_| &part[3..])
        else {
            continue;
        };
        let Some(xt) = percent_decode(xt) else {
            continue;
        };
        let urn = |p: &str| xt.get(..p.len()).is_some_and(|x| x.eq_ignore_ascii_case(p));
        if urn("urn:btih:") && h.v1.is_none() {
            let v = &xt[9..];
            h.v1 = match v.len() {
                40 => v.parse().ok(),
                32 => base32(v).map(InfoHash),
                _ => None,
            };
        } else if urn("urn:btmh:1220") && h.v2.is_none() {
            h.v2 = xt[13..].parse().ok();
        }
    }
    (h.v1.is_some() || h.v2.is_some())
        .then_some(h)
        .filter(|h| !is_zero(h))
}

/// An all-zero hash is a placeholder, never a torrent.
fn is_zero(h: &Hashes) -> bool {
    h.v1 == Some(InfoHash([0; 20])) || h.v2 == Some(InfoHashV2([0; 32]))
}

/// RFC 4648 base32 (case-insensitive, no padding) of exactly 20 bytes.
fn base32(s: &str) -> Option<[u8; 20]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 20];
    let (mut acc, mut bits, mut n) = (0u64, 0u32, 0usize);
    for b in s.bytes() {
        let v = match b.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        acc = (acc << 5) | u64::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out[n] = (acc >> bits) as u8;
            n += 1;
        }
    }
    Some(out)
}

pub(crate) fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_owned());
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A GUID or link fit to store as a provenance reference, or `None`.
///
/// Magnets are left out (the hash is the identity, and private trackers
/// put passkeys in their `tr` URLs), as are references over [`MAX_REF`]
/// bytes and ones with control characters. In an http(s) URL the user
/// info and every query parameter whose name looks like a credential
/// (`apikey`, `passkey`, `torrent_pass`, `token`, `auth`, …) are removed:
/// Prowlarr and Jackett download links carry the manager's API key.
/// Parameters split on `&` and `;`, and names are percent-decoded first.
/// A reference that isn't http(s) is kept only when it's an opaque id of
/// ASCII letters, digits and `.:_-`. Credentials in a path can't be told
/// apart and stay; private indexers' items are `private_source` and
/// never leave the machine (§18.7).
pub fn external_ref(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.len() > MAX_REF || is_magnet(s) || s.chars().any(char::is_control) {
        return None;
    }
    let scheme_len = ["http://", "https://"]
        .into_iter()
        .find(|p| s.get(..p.len()).is_some_and(|x| x.eq_ignore_ascii_case(p)))
        .map(str::len);
    let Some(scheme_len) = scheme_len else {
        // Anything else is kept only as an opaque id (`urn:uuid:…`, a
        // number): no query to hide a key in, nothing a page could run.
        let opaque = s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".:_-".contains(&b));
        return opaque.then(|| s.to_owned());
    };
    let (scheme, rest) = s.split_at(scheme_len);
    let rest = rest.split('#').next().unwrap_or_default();
    // The authority ends at any delimiter a browser or server might take
    // (`\` is `/` to a browser). After the user info, the host must be a
    // plain name, an IPv4 address or a bracketed IPv6 literal, with an
    // optional port; anything else could be read as another host.
    let auth_end = rest.find(['/', '?', '\\', '&', ';']).unwrap_or(rest.len());
    let (authority, rest) = rest.split_at(auth_end);
    let host = authority.rsplit('@').next().unwrap_or_default();
    if !plain_host(host) {
        return None;
    }
    // nZEDb-style links start the query with `&` (`/getnzb/x.nzb&i=1&r=key`).
    let (path, query) = match rest.find(['?', '&', ';']) {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };
    let mut out = format!("{scheme}{host}{path}");
    let kept: Vec<&str> = query
        .into_iter()
        .flat_map(|q| q.split(['&', ';']))
        .filter(|p| {
            let (name, value) = p.split_once('=').unwrap_or((p, ""));
            // A name or value that doesn't decode can't be checked, so it
            // goes; so does a value that is a URL or holds a credential.
            !p.is_empty()
                && percent_decode(name).is_some_and(|n| !is_credential(&n))
                && percent_decode(value).is_some_and(|v| {
                    !v.contains("://")
                        && !v
                            .split(['?', '&', ';'])
                            .any(|kv| kv.split_once('=').is_some_and(|(k, _)| is_credential(k)))
                })
        })
        .collect();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    Some(out)
}

/// `name`, `1.2.3.4` or `[::1]`, each with an optional `:port`.
fn plain_host(h: &str) -> bool {
    let (name, port) = match h.strip_prefix('[') {
        Some(v6) => match v6.split_once(']') {
            Some((addr, rest)) if !addr.is_empty() => {
                if !addr
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b".:".contains(&b))
                {
                    return false;
                }
                ("v6", rest)
            }
            _ => return false,
        },
        None => match h.find(':') {
            Some(at) => (&h[..at], &h[at..]),
            None => (h, ""),
        },
    };
    let port_ok = port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|p| (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()));
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        && port_ok
}

fn is_credential(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "key", "token", "auth", "pass", "secret", "sign", "session", "cookie",
    ]
    .iter()
    .any(|w| n.contains(w))
        || matches!(
            n.as_str(),
            "sid" | "uid" | "sig" | "pid" | "user" | "u" | "i" | "r" | "k" | "pk" | "h" | "hash"
        )
}

/// An RFC 822/2822 date (`Sun, 01 Nov 2026 12:34:56 +0000`) in Unix
/// seconds. The weekday is optional and unchecked; seconds are optional;
/// a two-digit year is 19xx or 20xx; zones are numeric or `GMT`/`UT`/
/// `UTC`/`Z` (indexers don't write the obsolete letter zones). Anything
/// else is `None`.
pub(crate) fn rfc2822(s: &str) -> Option<i64> {
    let s = s.split_once(',').map_or(s, |(_, rest)| rest);
    let mut it = s.split_ascii_whitespace();
    let day: i64 = small(it.next()?, 2)?;
    let month = month(it.next()?)?;
    let y = it.next()?;
    let year: i64 = match y.len() {
        4 => small(y, 4)?,
        2 => small(y, 2).map(|y: i64| if y < 50 { 2000 + y } else { 1900 + y })?,
        _ => return None,
    };
    let mut hms = it.next()?.split(':');
    let h: i64 = small(hms.next()?, 2)?;
    let m: i64 = small(hms.next()?, 2)?;
    let sec: i64 = hms.next().map_or(Some(0), |x| small(x, 2))?;
    if hms.next().is_some() || h > 23 || m > 59 || sec > 60 {
        return None;
    }
    let offset = zone(it.next().unwrap_or("GMT"))?;
    if !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + h * 3600 + m * 60 + sec.min(59) - offset)
}

/// An RFC 3339 date-time (`2026-11-01T12:34:56Z`, `…56.5+01:00`) in Unix
/// seconds, the fraction dropped: Atom's `published` and `updated`.
pub(crate) fn rfc3339(s: &str) -> Option<i64> {
    let fixed = |a: usize, b: usize| {
        s.get(a..b)
            .filter(|x| x.len() == b - a)
            .and_then(|x| small(x, b - a))
    };
    let sep = |at: usize, c: &[u8]| s.as_bytes().get(at).is_some_and(|b| c.contains(b));
    let ok = sep(4, b"-") && sep(7, b"-") && sep(10, b"Tt ") && sep(13, b":") && sep(16, b":");
    let (year, month, day) = (fixed(0, 4)?, fixed(5, 7)?, fixed(8, 10)?);
    let (h, m, sec) = (fixed(11, 13)?, fixed(14, 16)?, fixed(17, 19)?);
    let mut z = s.get(19..)?;
    if let Some(f) = z.strip_prefix('.') {
        let n = f.bytes().take_while(u8::is_ascii_digit).count();
        z = f.get(n..).filter(|_| n > 0)?;
    }
    let offset = match z {
        "Z" | "z" => 0,
        _ if z.len() == 6 && z.as_bytes()[3] == b':' => zone(&format!("{}{}", &z[..3], &z[4..]))?,
        _ => return None,
    };
    if !ok || !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }
    if h > 23 || m > 59 || sec > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + h * 3600 + m * 60 + sec.min(59) - offset)
}

/// Up to `max` ASCII digits.
pub(crate) fn small(s: &str, max: usize) -> Option<i64> {
    if s.is_empty() || s.len() > max || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn month(s: &str) -> Option<i64> {
    const M: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let p = M.iter().position(|m| s.eq_ignore_ascii_case(m))?;
    Some(p as i64 + 1)
}

/// A zone's offset east of UTC, in seconds.
fn zone(z: &str) -> Option<i64> {
    if ["GMT", "UT", "UTC", "Z"]
        .iter()
        .any(|g| z.eq_ignore_ascii_case(g))
    {
        return Some(0);
    }
    let (sign, digits) = match z.as_bytes().first()? {
        b'+' => (1, &z[1..]),
        b'-' => (-1, &z[1..]),
        _ => return None,
    };
    let hh = small(digits.get(..2)?, 2)?;
    let mm = small(digits.get(2..)?, 2)?;
    (digits.len() == 4 && hh <= 23 && mm <= 59).then_some(sign * (hh * 3600 + mm * 60))
}

pub(crate) fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(rfc2822("Thu, 01 Jan 1970 00:00:00 +0000"), Some(0));
        assert_eq!(
            rfc2822("Sun, 01 Nov 2026 12:34:56 +0000"),
            Some(1_793_536_496)
        );
        assert_eq!(rfc2822("1 Nov 2026 13:34:56 +0100"), Some(1_793_536_496));
        assert_eq!(rfc2822("Sun, 01 Nov 2026 07:34 -0500"), Some(1_793_536_440));
        assert_eq!(rfc2822("29 Feb 2023 00:00:00 GMT"), None);
        assert!(rfc2822("29 Feb 2024 00:00:00 GMT").is_some());
        assert_eq!(rfc2822("01 Nov 2026 24:00:00 GMT"), None);
        assert_eq!(rfc2822("2026-11-01T12:00:00Z"), None);
        assert_eq!(rfc2822("01 Nov 2026 12:00:00 +99999"), None);
        assert_eq!(rfc2822(""), None);
        assert_eq!(rfc3339("2026-11-01T12:34:56Z"), Some(1_793_536_496));
        assert_eq!(
            rfc3339("2026-11-01t13:34:56.789+01:00"),
            Some(1_793_536_496)
        );
        assert_eq!(rfc3339("2026-11-01T07:34:56-05:00"), Some(1_793_536_496));
        for bad in [
            "2026-11-01T12:34:56",
            "2026-13-01T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2026-11-01T12:34:56.Z",
            "2026-11-01T12:34:56+0100",
            "2026-11-01T24:00:00Z",
            "+026-11-01T12:34:56Z",
            "2026-11-01T12:34:5é",
            "",
        ] {
            assert_eq!(rfc3339(bad), None, "{bad}");
        }
    }

    #[test]
    fn hashes() {
        let hex = "c9e15763f722f23e98a29decdfae341b98d53056";
        let h: InfoHash = hex.parse().unwrap();
        let v1 = Some(Hashes {
            v1: Some(h),
            v2: None,
        });
        assert_eq!(hash(hex), v1);
        assert_eq!(hash("ZHQVOY7XELZD5GFCTXWN7LRUDOMNKMCW"), v1);
        assert_eq!(hash("zhqvoy7xelzd5gfctxwn7lrudomnkmcw"), v1);
        assert_eq!(hash("ZHQVOY7XELZD5GFCTXWN7LRUDOMNKMC1"), None);
        assert_eq!(hash(&hex[1..]), None);
        let v2 = "a".repeat(64);
        let only_v2 = hash(&v2).unwrap();
        assert_eq!(
            (only_v2.v1, only_v2.v2.map(|h| h.0)),
            (None, Some([0xaa; 32]))
        );
        assert_eq!(only_v2.key(), Some(InfoHash([0xaa; 20])));
        let m = format!("magnet:?xt=urn%3Abtih%3A{hex}&dn=x&xt=urn:btmh:1220{v2}");
        let got = magnet_hashes(&m).unwrap();
        assert_eq!(got.v1, Some(h));
        assert_eq!(got.v2.map(|h| h.0), Some([0xaa; 32]));
        assert_eq!(magnet_hashes("magnet:?xt=urn:btih:nope"), None);
        assert_eq!(magnet_hashes("magnet:?xt=urn:btih:%zz"), None);
        assert_eq!(percent_decode("%+f"), None);
        assert_eq!(hash(&"0".repeat(40)), None);
        assert_eq!(hash(&"0".repeat(64)), None);
        assert_eq!(
            magnet_hashes(&format!("magnet:?xt=urn:btih:{}", "0".repeat(40))),
            None
        );
        assert_eq!(
            clean_title("a\u{202E}b\u{2066}c\u{200F}d\u{061C}e\u{1}"),
            "abcde"
        );
        let v2h: InfoHashV2 = v2.parse().unwrap();
        let attr = Hashes {
            v1: Some(h),
            v2: None,
        };
        let both = Hashes {
            v1: Some(h),
            v2: Some(v2h),
        };
        assert_eq!(merge(attr, Some(both)), both);
        let other = Some(Hashes {
            v1: Some(InfoHash([1; 20])),
            v2: Some(v2h),
        });
        assert_eq!(
            merge(attr, other),
            attr,
            "a disagreeing magnet adds nothing"
        );
    }

    #[test]
    fn references_lose_credentials() {
        assert_eq!(
            external_ref(
                "http://u:p@prowlarr.local:9696/1/download?apikey=SECRET&link=abc&file=X#f"
            )
            .as_deref(),
            Some("http://prowlarr.local:9696/1/download?link=abc&file=X")
        );
        assert_eq!(
            external_ref(
                "https://t.example/details.php?id=7&passkey=S&torrent_pass=S&jackett_apikey=S"
            )
            .as_deref(),
            Some("https://t.example/details.php?id=7")
        );
        assert_eq!(
            external_ref("HTTP://a/?token=x").as_deref(),
            Some("HTTP://a/")
        );
        assert_eq!(
            external_ref("https://nzb.example/getnzb/ab.nzb&i=1&r=KEY").as_deref(),
            Some("https://nzb.example/getnzb/ab.nzb")
        );
        // A password with `&` ends the authority early; the leftover
        // isn't a plain host, so nothing is kept.
        assert_eq!(external_ref("http://user:p&ss=x@h.example/d?id=1"), None);
        assert_eq!(
            external_ref("https://t.example/d?id=1;passkey=S&api%6Bey=S&hash=S").as_deref(),
            Some("https://t.example/d?id=1")
        );
        assert_eq!(external_ref("//h.example/dl?apikey=K"), None);
        assert_eq!(
            external_ref("http://h.example&apikey=K").as_deref(),
            Some("http://h.example")
        );
        assert_eq!(
            external_ref("https://h.example;passkey=K").as_deref(),
            Some("https://h.example")
        );
        assert_eq!(
            external_ref("https://h.example\\@evil/d?apikey=K").as_deref(),
            Some("https://h.example\\@evil/d")
        );
        assert_eq!(external_ref("https://h.exa mple/"), None);
        assert_eq!(external_ref("https://h.example:99999999/"), None);
        assert_eq!(
            external_ref("http://[::1]:9696/x").as_deref(),
            Some("http://[::1]:9696/x")
        );
        // Credentials inside a value: a wrapped URL, or a key=value pair.
        assert_eq!(
            external_ref(
                "https://p.example/dl?link=https%3A%2F%2Ft%2Fd%3Fid%3D1&id=2&x=a%26passkey%3DS"
            )
            .as_deref(),
            Some("https://p.example/dl?id=2")
        );
        assert_eq!(external_ref("javascript:alert(1)"), None);
        assert_eq!(
            external_ref("urn:uuid:1234").as_deref(),
            Some("urn:uuid:1234")
        );
        assert_eq!(external_ref("magnet:?xt=urn:btih:00"), None);
        assert_eq!(external_ref(&"a".repeat(MAX_REF + 1)), None);
        assert_eq!(external_ref("a\u{1}b"), None);
    }
}
