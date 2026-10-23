//! Trusted reverse-proxy header auth (§12), off by default.
//!
//! - A request is signed in by the user header (default `Remote-User`)
//!   **only when its TCP peer is a trusted proxy** and proxy auth is on.
//!   From anyone else the header is ignored, so a client can't sign
//!   itself in by sending it. Cookies keep working either way.
//! - Behind a trusted proxy, the client address (for per-client login
//!   and key limits) is the rightmost `X-Forwarded-For` entry that isn't
//!   a trusted proxy. Entries left of it are the client's own claims and
//!   are never read. An entry that doesn't parse stops the walk at the
//!   last trusted hop, since the trusted proxies only append valid ones.
//! - With proxy auth off, every proxy header is ignored, the forwarded
//!   address included.
//! - `LODESTAR_PROXY_AUTH=off` ([`PROXY_AUTH_ENV`]) forces it off for one
//!   run, without changing the saved settings: the way back in when a
//!   proxy change locks you out.
//!
//! The user header's value is never logged or echoed.

use std::net::IpAddr;
use std::str::FromStr;

use axum::http::{HeaderMap, HeaderName, header};
use serde::{Deserialize, Serialize};

/// The environment variable that forces proxy auth off (`off`).
pub const PROXY_AUTH_ENV: &str = "LODESTAR_PROXY_AUTH";
pub const DEFAULT_USER_HEADER: &str = "Remote-User";
/// At most this many trusted proxies and allowed users.
pub const MAX_ENTRIES: usize = 64;
/// A user name is at most this many bytes.
pub const MAX_USER_BYTES: usize = 256;
/// A header name is at most this many bytes.
const MAX_HEADER_BYTES: usize = 64;
/// `X-Forwarded-For` entries read from the right, at most.
const MAX_HOPS: usize = 16;
/// One `X-Forwarded-For` entry is at most this many bytes.
const MAX_ENTRY_BYTES: usize = 64;

/// Headers that carry something else and can't name the user.
const RESERVED: [&str; 17] = [
    "authorization",
    "connection",
    "content-length",
    "content-type",
    "cookie",
    "forwarded",
    "host",
    "origin",
    "proxy-authorization",
    "referer",
    "set-cookie",
    "transfer-encoding",
    "x-api-key",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
];

/// The saved settings, as `auth.json` and `/auth/proxy` hold them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub enabled: bool,
    /// IPs and CIDRs, canonical (`10.0.0.0/8`, `192.168.1.2/32`).
    pub trusted: Vec<String>,
    pub header: String,
    /// Empty: any user the proxy names.
    pub allowed_users: Vec<String>,
}

impl Default for ProxyConfig {
    fn default() -> ProxyConfig {
        ProxyConfig {
            enabled: false,
            trusted: Vec::new(),
            header: DEFAULT_USER_HEADER.to_string(),
            allowed_users: Vec::new(),
        }
    }
}

impl ProxyConfig {
    pub(crate) fn is_default(&self) -> bool {
        *self == ProxyConfig::default()
    }
}

/// Why a [`ProxyConfig`] was refused: the field and what's wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyError {
    pub field: &'static str,
    pub message: &'static str,
}

fn refuse(field: &'static str, message: &'static str) -> ProxyError {
    ProxyError { field, message }
}

/// [`ProxyConfig`] checked and compiled.
#[derive(Debug, Clone)]
pub(crate) struct Proxy {
    pub(crate) config: ProxyConfig,
    trusted: Vec<Cidr>,
    header: HeaderName,
}

impl Default for Proxy {
    fn default() -> Proxy {
        Proxy::compile(ProxyConfig::default()).expect("the default is valid")
    }
}

impl Proxy {
    /// Checks `config` and puts it in canonical form.
    pub(crate) fn compile(config: ProxyConfig) -> Result<Proxy, ProxyError> {
        if config.trusted.len() > MAX_ENTRIES {
            return Err(refuse("trusted", "List at most 64 trusted proxies."));
        }
        let mut trusted: Vec<Cidr> = Vec::with_capacity(config.trusted.len());
        for t in &config.trusted {
            let c = Cidr::parse(t).ok_or(refuse(
                "trusted",
                "Each trusted proxy is an IP address or a CIDR range, such as 172.17.0.1 or 10.0.0.0/8.",
            ))?;
            if c.len == 0 {
                return Err(refuse(
                    "trusted",
                    "A /0 range trusts every address, so anyone could sign in by sending the header.",
                ));
            }
            // A request with no known peer reads as the unspecified
            // address; a range holding it would trust those.
            if c.contains(IpAddr::from([0, 0, 0, 0])) || c.contains(IpAddr::from([0u16; 8])) {
                return Err(refuse(
                    "trusted",
                    "A range that includes 0.0.0.0 or :: can't be trusted; list your proxy's own address.",
                ));
            }
            if !trusted.contains(&c) {
                trusted.push(c);
            }
        }
        let name = config.header.trim();
        let header = HeaderName::from_str(name)
            .ok()
            .filter(|_| !name.is_empty() && name.len() <= MAX_HEADER_BYTES)
            .ok_or(refuse(
                "header",
                "The user header must be a valid header name.",
            ))?;
        if RESERVED.contains(&header.as_str()) {
            return Err(refuse(
                "header",
                "That header carries something else; use the one your proxy sets for the user, such as Remote-User.",
            ));
        }
        if config.allowed_users.len() > MAX_ENTRIES {
            return Err(refuse("allowed_users", "List at most 64 users."));
        }
        let mut users: Vec<String> = Vec::with_capacity(config.allowed_users.len());
        for u in &config.allowed_users {
            let u = user_name(u).ok_or(refuse(
                "allowed_users",
                "Each user name is 1 to 256 bytes with no control characters.",
            ))?;
            if !users.iter().any(|x| x == u) {
                users.push(u.to_string());
            }
        }
        if config.enabled && trusted.is_empty() {
            return Err(refuse(
                "trusted",
                "Name the proxy that signs people in before turning this on.",
            ));
        }
        Ok(Proxy {
            config: ProxyConfig {
                enabled: config.enabled,
                trusted: trusted.iter().map(Cidr::to_string).collect(),
                // As typed, so the screen shows what the proxy docs say.
                header: name.to_string(),
                allowed_users: users,
            },
            trusted,
            header,
        })
    }

    /// Whether a trusted range is wider than a /16 (IPv4) or /48 (IPv6):
    /// worth a warning, since every host in it is believed.
    pub(crate) fn has_wide_range(&self) -> bool {
        self.trusted
            .iter()
            .any(|c| c.len < if c.net.is_ipv4() { 16 } else { 48 })
    }

    pub(crate) fn trusts(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        self.trusted.iter().any(|c| c.contains(ip))
    }

    /// Whether the user header signs this request in: proxy auth is on,
    /// the TCP peer is trusted, and the header holds exactly one allowed
    /// user name.
    pub(crate) fn signs_in(&self, peer: IpAddr, headers: &HeaderMap) -> bool {
        if !self.config.enabled || !self.trusts(peer) {
            return false;
        }
        let mut values = headers.get_all(&self.header).iter();
        let (Some(value), None) = (values.next(), values.next()) else {
            return false;
        };
        let Some(user) = value.to_str().ok().and_then(user_name) else {
            return false;
        };
        self.config.allowed_users.is_empty() || self.config.allowed_users.iter().any(|u| u == user)
    }

    /// The client's address: the peer, or behind a trusted proxy the
    /// rightmost forwarded address that isn't one.
    pub(crate) fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        if !self.config.enabled {
            return canonical(peer);
        }
        let values: Vec<&str> = headers
            .get_all(header::HeaderName::from_static("x-forwarded-for"))
            .iter()
            .map(|v| v.to_str().unwrap_or(""))
            .collect();
        forwarded_client(peer, &values, |ip| self.trusts(ip))
    }
}

/// The walk behind [`Proxy::client_ip`], over the header's values in the
/// order received. Never returns an address `trusted` accepts, except the
/// peer itself.
pub(crate) fn forwarded_client(
    peer: IpAddr,
    values: &[&str],
    trusted: impl Fn(IpAddr) -> bool,
) -> IpAddr {
    let peer = canonical(peer);
    if !trusted(peer) {
        return peer;
    }
    let entries = values.iter().rev().flat_map(|v| v.rsplit(','));
    for entry in entries.take(MAX_HOPS) {
        let Some(ip) = forwarded_entry(entry) else {
            return peer;
        };
        if !trusted(ip) {
            return ip;
        }
    }
    peer
}

/// One `X-Forwarded-For` entry: an address, optionally quoted, bracketed
/// or with a port (`203.0.113.7`, `"[2001:db8::1]:443"`).
fn forwarded_entry(entry: &str) -> Option<IpAddr> {
    let e = entry.trim();
    if e.len() > MAX_ENTRY_BYTES {
        return None;
    }
    let e = e
        .strip_prefix('"')
        .and_then(|e| e.strip_suffix('"'))
        .unwrap_or(e);
    let ip = IpAddr::from_str(e)
        .ok()
        .or_else(|| std::net::SocketAddr::from_str(e).ok().map(|s| s.ip()))
        .or_else(|| {
            e.strip_prefix('[')
                .and_then(|e| e.strip_suffix(']'))
                .and_then(|e| e.parse().ok())
        })?;
    Some(canonical(ip))
}

/// A user name as the header carries it: trimmed, 1–256 bytes, no
/// control characters.
fn user_name(raw: &str) -> Option<&str> {
    let u = raw.trim();
    (!u.is_empty() && u.len() <= MAX_USER_BYTES && !u.chars().any(char::is_control)).then_some(u)
}

/// IPv4-mapped IPv6 (`::ffff:10.0.0.1`) as plain IPv4, so a dual-stack
/// listener's peers match IPv4 entries.
pub(crate) fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cidr {
    net: IpAddr,
    len: u8,
}

impl Cidr {
    fn parse(s: &str) -> Option<Cidr> {
        let s = s.trim();
        let (addr, len) = match s.split_once('/') {
            Some((a, l))
                if !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()) && l.len() <= 3 =>
            {
                (a, Some(l.parse::<u8>().ok()?))
            }
            Some(_) => return None,
            None => (s, None),
        };
        let ip = canonical(IpAddr::from_str(addr).ok()?);
        let max = if ip.is_ipv4() { 32 } else { 128 };
        // A mapped address's prefix counts the 96 mapping bits.
        let len = match (len, IpAddr::from_str(addr).ok()?) {
            (Some(l), IpAddr::V6(v6)) if v6.to_ipv4_mapped().is_some() => l.checked_sub(96)?,
            (Some(l), _) => l,
            (None, _) => max,
        };
        if len > max {
            return None;
        }
        Some(Cidr {
            net: mask(ip, len),
            len,
        })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        ip.is_ipv4() == self.net.is_ipv4() && mask(ip, self.len) == self.net
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.net, self.len)
    }
}

fn mask(ip: IpAddr, len: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let m = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
            IpAddr::V4((bits & m).into())
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let m = u128::MAX.checked_shl(128 - u32::from(len)).unwrap_or(0);
            IpAddr::V6((bits & m).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use proptest::prelude::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn proxy(trusted: &[&str], users: &[&str]) -> Proxy {
        Proxy::compile(ProxyConfig {
            enabled: true,
            trusted: trusted.iter().map(|s| s.to_string()).collect(),
            header: DEFAULT_USER_HEADER.into(),
            allowed_users: users.iter().map(|s| s.to_string()).collect(),
        })
        .unwrap()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                HeaderName::from_str(k).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn cidrs_parse_mask_and_match() {
        let c = Cidr::parse("10.1.2.3/8").unwrap();
        assert_eq!(c.to_string(), "10.0.0.0/8");
        assert!(c.contains(ip("10.200.0.1")) && !c.contains(ip("11.0.0.1")));
        assert_eq!(
            Cidr::parse("192.168.1.2").unwrap().to_string(),
            "192.168.1.2/32"
        );
        assert_eq!(Cidr::parse("fd00::1/8").unwrap().to_string(), "fd00::/8");
        assert_eq!(
            Cidr::parse("::ffff:10.0.0.0/104").unwrap().to_string(),
            "10.0.0.0/8"
        );
        assert!(
            !Cidr::parse("::/0").unwrap().contains(ip("10.0.0.1")),
            "families never mix"
        );
        for bad in [
            "",
            "10.0.0.0/33",
            "10.0.0.0/",
            "10.0.0.0/+8",
            "::/129",
            "host",
            "10.0.0.0/8/8",
            "::ffff:1.2.3.4/90",
        ] {
            assert!(Cidr::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn compile_checks_and_canonicalizes() {
        let p = proxy(
            &["10.1.0.0/16", "10.1.2.3/16", " 172.17.0.1 "],
            &[" ann ", "ann", "bo"],
        );
        assert_eq!(p.config.trusted, ["10.1.0.0/16", "172.17.0.1/32"]);
        assert_eq!(p.config.allowed_users, ["ann", "bo"]);
        let bad = |c: ProxyConfig| Proxy::compile(c).unwrap_err().field;
        let base = ProxyConfig {
            enabled: true,
            trusted: vec!["10.0.0.1".into()],
            ..ProxyConfig::default()
        };
        assert_eq!(
            bad(ProxyConfig {
                trusted: vec!["0.0.0.0/0".into()],
                ..base.clone()
            }),
            "trusted"
        );
        for unspecified in ["0.0.0.0", "0.0.0.0/1", "::/1", "::", "::ffff:0.0.0.0/97"] {
            assert_eq!(
                bad(ProxyConfig {
                    trusted: vec![unspecified.into()],
                    ..base.clone()
                }),
                "trusted",
                "{unspecified}"
            );
        }
        assert!(!proxy(&["10.1.0.0/16", "fd00::/48"], &[]).has_wide_range());
        assert!(proxy(&["10.0.0.0/8"], &[]).has_wide_range());
        assert!(proxy(&["128.0.0.0/1"], &[]).has_wide_range());
        assert_eq!(
            bad(ProxyConfig {
                trusted: vec![],
                ..base.clone()
            }),
            "trusted"
        );
        assert_eq!(
            bad(ProxyConfig {
                trusted: vec!["nope".into()],
                ..base.clone()
            }),
            "trusted"
        );
        assert_eq!(
            bad(ProxyConfig {
                trusted: vec!["10.0.0.1".into(); 65],
                ..base.clone()
            }),
            "trusted"
        );
        for h in [
            "Cookie",
            "authorization",
            "X-Forwarded-For",
            "",
            "bad header",
            &"x".repeat(65),
        ] {
            assert_eq!(
                bad(ProxyConfig {
                    header: h.into(),
                    ..base.clone()
                }),
                "header",
                "{h:?}"
            );
        }
        assert_eq!(
            bad(ProxyConfig {
                allowed_users: vec!["".into()],
                ..base.clone()
            }),
            "allowed_users"
        );
        assert_eq!(
            bad(ProxyConfig {
                allowed_users: vec!["a\u{7}".into()],
                ..base.clone()
            }),
            "allowed_users"
        );
        // Off, an empty list is fine.
        assert!(Proxy::compile(ProxyConfig::default()).is_ok());
    }

    #[test]
    fn the_header_signs_in_only_from_a_trusted_peer() {
        let p = proxy(&["172.17.0.0/16"], &[]);
        let h = headers(&[("remote-user", "ann")]);
        assert!(p.signs_in(ip("172.17.0.2"), &h));
        assert!(
            p.signs_in(ip("::ffff:172.17.0.2"), &h),
            "mapped peers match"
        );
        assert!(!p.signs_in(ip("192.168.1.9"), &h), "untrusted peer");
        assert!(!p.signs_in(ip("172.17.0.2"), &headers(&[])));
        assert!(!p.signs_in(ip("172.17.0.2"), &headers(&[("remote-user", "  ")])));
        assert!(
            !p.signs_in(
                ip("172.17.0.2"),
                &headers(&[("remote-user", "a"), ("remote-user", "b")])
            ),
            "two values are ambiguous"
        );
        let only = proxy(&["172.17.0.0/16"], &["ann"]);
        assert!(only.signs_in(ip("172.17.0.2"), &h));
        assert!(!only.signs_in(ip("172.17.0.2"), &headers(&[("remote-user", "eve")])));
        let mut off = p.clone();
        off.config.enabled = false;
        assert!(!off.signs_in(ip("172.17.0.2"), &h));
    }

    #[test]
    fn the_client_is_the_rightmost_untrusted_hop() {
        let p = proxy(&["10.0.0.0/8"], &[]);
        let peer = ip("10.0.0.1");
        let at = |xff: &[&str]| {
            let pairs: Vec<(&str, &str)> = xff.iter().map(|v| ("x-forwarded-for", *v)).collect();
            p.client_ip(peer, &headers(&pairs))
        };
        assert_eq!(at(&["203.0.113.7"]), ip("203.0.113.7"));
        assert_eq!(
            at(&["6.6.6.6, 203.0.113.7, 10.0.0.5"]),
            ip("203.0.113.7"),
            "a spoofed left entry is never read"
        );
        assert_eq!(
            at(&["6.6.6.6", "203.0.113.7"]),
            ip("203.0.113.7"),
            "headers join in order"
        );
        assert_eq!(at(&["\"[2001:db8::1]:443\""]), ip("2001:db8::1"));
        assert_eq!(at(&["203.0.113.7:5555"]), ip("203.0.113.7"));
        assert_eq!(at(&["::ffff:203.0.113.7"]), ip("203.0.113.7"));
        assert_eq!(at(&[]), peer);
        assert_eq!(at(&["10.0.0.9"]), peer, "all trusted: the peer");
        assert_eq!(
            at(&["203.0.113.7, unknown"]),
            peer,
            "a bad hop stops the walk"
        );
        assert_eq!(
            at(&[&format!("203.0.113.7{}", ", 10.0.0.2".repeat(20))]),
            peer,
            "hops are capped"
        );
        // From an untrusted peer the header is ignored.
        let h = headers(&[("x-forwarded-for", "203.0.113.7")]);
        assert_eq!(p.client_ip(ip("192.168.1.5"), &h), ip("192.168.1.5"));
        let mut off = p.clone();
        off.config.enabled = false;
        assert_eq!(off.client_ip(peer, &h), peer, "off ignores it too");
        assert_eq!(
            off.client_ip(ip("::ffff:10.0.0.1"), &h),
            peer,
            "and canonicalizes"
        );
    }

    proptest! {
        /// Whatever the header says, the walk never panics and never
        /// names a trusted proxy as the client (the peer aside).
        #[test]
        fn forwarded_for_never_yields_a_trusted_proxy(
            values in proptest::collection::vec(".{0,300}", 0..4),
            octets in proptest::collection::vec(any::<u8>(), 0..64),
            peer_trusted in any::<bool>(),
        ) {
            let p = proxy(&["10.0.0.0/8", "fd00::/8", "192.168.1.1"], &[]);
            let peer = if peer_trusted { ip("10.9.9.9") } else { ip("8.8.8.8") };
            // Mix in well-formed entries, trusted and not, so the walk goes deep.
            let mut mixed: Vec<String> = values.clone();
            mixed.push(octets.chunks(4).map(|c| c.iter().map(u8::to_string).collect::<Vec<_>>().join(".")).collect::<Vec<_>>().join(", "));
            let refs: Vec<&str> = mixed.iter().map(String::as_str).collect();
            let got = forwarded_client(peer, &refs, |i| p.trusts(i));
            prop_assert!(got == peer || !p.trusts(got), "{got} from {refs:?}");
            if !peer_trusted {
                prop_assert_eq!(got, peer);
            }
        }
    }
}
