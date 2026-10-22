//! Magnet links handed to download clients.
//!
//! A magnet comes from Lodestar's own index, but it can also come from a
//! user or a source plugin, so it's parsed as untrusted input. Two things
//! matter downstream:
//! - **No whitespace or control characters.** Several clients take a list
//!   of magnets separated by newlines (qBittorrent's `urls` field); one
//!   magnet with an embedded newline would smuggle in another URL.
//! - **A torrent ID** the client uses in its API afterwards: the v1
//!   info-hash in lowercase hex, or for a v2-only magnet the first 20 bytes
//!   of the v2 hash (qBittorrent's "torrent ID").

use std::fmt;

/// A checked magnet link.
#[derive(Clone, PartialEq, Eq)]
pub struct Magnet {
    uri: String,
    id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagnetError {
    /// Not a `magnet:?` URI.
    NotMagnet,
    /// Longer than [`Magnet::MAX_LEN`].
    TooLong,
    /// Has whitespace, control or non-ASCII characters (they must be
    /// percent-encoded).
    BadCharacters,
    /// No `xt=urn:btih:` or `xt=urn:btmh:` with a valid hash.
    NoInfoHash,
}

impl fmt::Display for MagnetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MagnetError::NotMagnet => "not a magnet link",
            MagnetError::TooLong => "the magnet link is too long",
            MagnetError::BadCharacters => "the magnet link has unencoded characters",
            MagnetError::NoInfoHash => "the magnet link has no valid info-hash",
        })
    }
}

impl std::error::Error for MagnetError {}

impl Magnet {
    /// The longest magnet accepted (trackers lists make them long, but not
    /// this long).
    pub const MAX_LEN: usize = 16 * 1024;

    pub fn parse(uri: &str) -> Result<Magnet, MagnetError> {
        if uri.len() > Self::MAX_LEN {
            return Err(MagnetError::TooLong);
        }
        let scheme = uri.get(..8).ok_or(MagnetError::NotMagnet)?;
        if !scheme.eq_ignore_ascii_case("magnet:?") {
            return Err(MagnetError::NotMagnet);
        }
        if !uri.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(MagnetError::BadCharacters);
        }
        let mut v1 = None;
        let mut v2 = None;
        for pair in uri[8..].split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            // `xt`, or `xt.1`, `xt.2`… for several exact topics.
            let key = key.to_ascii_lowercase();
            if key != "xt" && !key.strip_prefix("xt.").is_some_and(|n| !n.is_empty()) {
                continue;
            }
            let value = percent_decode(value).ok_or(MagnetError::NoInfoHash)?;
            let lower = value.to_ascii_lowercase();
            if let Some(hash) = lower.strip_prefix("urn:btih:") {
                v1 = v1.or(btih(hash));
            } else if let Some(hash) = lower.strip_prefix("urn:btmh:") {
                v2 = v2.or(btmh(hash));
            }
        }
        let id = v1.or(v2).ok_or(MagnetError::NoInfoHash)?;
        Ok(Magnet {
            uri: uri.to_owned(),
            id,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.uri
    }

    /// The torrent ID a client's API uses: 40 lowercase hex digits.
    pub fn torrent_id(&self) -> &str {
        &self.id
    }
}

impl fmt::Debug for Magnet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Magnet({})", self.id)
    }
}

/// A v1 hash: 40 hex digits, or 32 base32 characters.
fn btih(hash: &str) -> Option<String> {
    match hash.len() {
        40 if hash.bytes().all(|b| b.is_ascii_hexdigit()) => Some(hash.to_owned()),
        32 => base32_to_hex(hash),
        _ => None,
    }
}

/// A v2 multihash: `1220` (SHA-256, 32 bytes) and 64 hex digits. The ID is
/// the first 20 bytes.
fn btmh(hash: &str) -> Option<String> {
    let digest = hash.strip_prefix("1220")?;
    (digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| digest[..40].to_owned())
}

fn base32_to_hex(s: &str) -> Option<String> {
    let mut bits: u64 = 0;
    let mut n = 0;
    let mut out = String::with_capacity(40);
    for c in s.bytes() {
        let v = match c {
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        bits = (bits << 5) | u64::from(v);
        n += 5;
        while n >= 4 {
            n -= 4;
            out.push(char::from_digit(((bits >> n) & 0xf) as u32, 16)?);
        }
    }
    (out.len() == 40).then_some(out)
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const HEX: &str = "c9e15763f722f23e98a29decdfae341b98d53056";

    #[test]
    fn parses_v1_hex_and_base32() {
        let m = Magnet::parse(&format!("magnet:?xt=urn:btih:{HEX}&dn=A+Name")).unwrap();
        assert_eq!(m.torrent_id(), HEX);
        let upper =
            Magnet::parse(&format!("MAGNET:?dn=x&XT=urn:BTIH:{}", HEX.to_uppercase())).unwrap();
        assert_eq!(upper.torrent_id(), HEX);
        // The same hash in base32.
        let b32 = Magnet::parse("magnet:?xt=urn:btih:ZHQVOY7XELZD5GFCTXWN7LRUDOMNKMCW").unwrap();
        assert_eq!(b32.torrent_id(), HEX);
        let encoded = Magnet::parse(&format!("magnet:?xt=urn%3Abtih%3A{HEX}")).unwrap();
        assert_eq!(encoded.torrent_id(), HEX);
        let numbered = Magnet::parse(&format!("magnet:?xt.1=urn:btih:{HEX}")).unwrap();
        assert_eq!(numbered.torrent_id(), HEX);
    }

    #[test]
    fn parses_v2_and_hybrid() {
        let v2 = "1220".to_owned() + &"ab".repeat(32);
        let m = Magnet::parse(&format!("magnet:?xt=urn:btmh:{v2}")).unwrap();
        assert_eq!(m.torrent_id(), "ab".repeat(20));
        let hybrid = Magnet::parse(&format!("magnet:?xt=urn:btmh:{v2}&xt=urn:btih:{HEX}")).unwrap();
        assert_eq!(hybrid.torrent_id(), HEX, "a hybrid is known by its v1 hash");
    }

    #[test]
    fn refuses_bad_magnets() {
        for (uri, want) in [
            ("", MagnetError::NotMagnet),
            ("http://x/", MagnetError::NotMagnet),
            ("magnet:", MagnetError::NotMagnet),
            ("magnet:?dn=x", MagnetError::NoInfoHash),
            ("magnet:?xt=urn:btih:abc", MagnetError::NoInfoHash),
            ("magnet:?xt=urn:sha1:abc", MagnetError::NoInfoHash),
            ("magnet:?xt=urn:btih:%zz", MagnetError::NoInfoHash),
        ] {
            assert_eq!(Magnet::parse(uri), Err(want), "{uri:?}");
        }
        let injected = format!("magnet:?xt=urn:btih:{HEX}\nhttp://evil.example/x.torrent");
        assert_eq!(Magnet::parse(&injected), Err(MagnetError::BadCharacters));
        let spaced = format!("magnet:?xt=urn:btih:{HEX}&dn=a name");
        assert_eq!(Magnet::parse(&spaced), Err(MagnetError::BadCharacters));
        let long = format!("magnet:?xt=urn:btih:{HEX}&tr={}", "a".repeat(20_000));
        assert_eq!(Magnet::parse(&long), Err(MagnetError::TooLong));
        assert_eq!(Magnet::parse("magnét:?xt"), Err(MagnetError::NotMagnet));
    }

    proptest! {
        #[test]
        fn never_panics_and_accepted_magnets_are_one_clean_line(s in "\\PC{0,200}") {
            if let Ok(m) = Magnet::parse(&s) {
                prop_assert!(m.as_str().bytes().all(|b| b.is_ascii_graphic()));
                prop_assert_eq!(m.torrent_id().len(), 40);
            }
        }

        #[test]
        fn any_hash_with_any_extra_params_parses(
            hash in "[0-9a-f]{40}",
            extra in "[a-zA-Z0-9.%=&+_-]{0,100}",
        ) {
            let m = Magnet::parse(&format!("magnet:?xt=urn:btih:{hash}&{extra}"));
            // An `extra` with its own xt can only add a second topic, and
            // the first valid v1 hash wins.
            if let Ok(m) = m {
                prop_assert_eq!(m.torrent_id(), hash.as_str());
            }
        }

        #[test]
        fn arbitrary_bytes_after_the_scheme(rest in proptest::collection::vec(any::<u8>(), 0..300)) {
            let s = format!("magnet:?{}", String::from_utf8_lossy(&rest));
            let _ = Magnet::parse(&s);
        }
    }
}
