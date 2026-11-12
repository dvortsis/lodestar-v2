//! The SOCKS5 client handshake (RFC 1928, RFC 1929) for routed raw TCP
//! connections ([`super::connect_tcp_routed`]).
//!
//! Everything a proxy sends is untrusted input, so the parsers here are
//! pure functions over bytes, bounded and panic-free:
//!
//! - **Exact reads.** The reply to `CONNECT` has a length fixed by its
//!   address type (at most [`MAX_REPLY`] bytes). The handshake reads the
//!   four-byte head plus one byte, learns the full length from
//!   [`reply_len`], and reads exactly the rest, so no byte of the
//!   tunnelled stream is ever consumed by the handshake.
//! - **Strict replies.** The version must be 5, the reserved byte 0, the
//!   address type one of the three RFC 1928 defines, and a domain-name
//!   address 1 to 255 bytes of printable ASCII. Anything else is
//!   [`SocksError::Malformed`].
//! - **Only what was offered.** The proxy must pick a method the client
//!   offered: no authentication only when no credentials are configured,
//!   user name and password only when they are.
//! - **No secrets in errors.** No variant carries a credential or a byte
//!   the proxy sent beyond its reply code.

use std::error::Error as StdError;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

/// The protocol version byte.
pub const VERSION: u8 = 5;
/// The RFC 1929 sub-negotiation version byte.
pub const AUTH_VERSION: u8 = 1;
/// Method: no authentication.
pub const METHOD_NONE: u8 = 0x00;
/// Method: user name and password (RFC 1929).
pub const METHOD_USERPASS: u8 = 0x02;
/// Method reply: none of the offered methods is acceptable.
pub const METHOD_REFUSED: u8 = 0xff;
/// Command: `CONNECT`.
pub const CMD_CONNECT: u8 = 0x01;
/// Address type: IPv4.
pub const ATYP_V4: u8 = 0x01;
/// Address type: a domain name.
pub const ATYP_DOMAIN: u8 = 0x03;
/// Address type: IPv6.
pub const ATYP_V6: u8 = 0x04;
/// The longest reply to `CONNECT`: head (4), length byte (1), a 255-byte
/// name and the port (2).
pub const MAX_REPLY: usize = 4 + 1 + 255 + 2;
/// How many bytes [`reply_len`] needs to know a reply's full length.
pub const REPLY_PREFIX: usize = 5;

/// Why a SOCKS5 handshake failed. Carries no credential and no proxy
/// bytes except the RFC 1928 reply code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocksError {
    /// The proxy's bytes don't follow RFC 1928/1929.
    Malformed(&'static str),
    /// The proxy accepted none of the offered methods, or picked one that
    /// wasn't offered.
    NoAcceptableMethod,
    /// The proxy refused the user name and password.
    AuthRejected,
    /// The proxy answered `CONNECT` with a failure code (RFC 1928 §6).
    Refused(u8),
    /// A user name, password or destination name is empty or longer than
    /// 255 bytes.
    TooLong,
}

impl fmt::Display for SocksError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SocksError::Malformed(why) => write!(f, "the proxy's answer is malformed: {why}"),
            SocksError::NoAcceptableMethod => {
                f.write_str("the proxy wants an authentication method Lodestar didn't offer")
            }
            SocksError::AuthRejected => f.write_str("the proxy refused the user name and password"),
            SocksError::Refused(code) => {
                write!(f, "the proxy refused the connection: {}", refusal(*code))
            }
            SocksError::TooLong => {
                f.write_str("a user name, password or host name is empty or over 255 bytes")
            }
        }
    }
}

impl StdError for SocksError {}

/// RFC 1928 §6's reply codes, in words.
fn refusal(code: u8) -> &'static str {
    match code {
        0x01 => "general failure",
        0x02 => "not allowed by its rules",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "TTL expired",
        0x07 => "command not supported",
        0x08 => "address type not supported",
        _ => "an unassigned code",
    }
}

/// What the client asks the proxy to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A checked address.
    Addr(SocketAddr),
    /// A name the proxy resolves (`socks5h`): 1 to 255 bytes, already
    /// normalised and checked by the caller.
    Name(String, u16),
}

/// The address a proxy reports in its reply (`BND.ADDR`, `BND.PORT`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bound {
    Addr(SocketAddr),
    Name(String, u16),
}

/// The method the proxy chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    NoAuth,
    UserPass,
}

/// The client greeting: version, then one method, user name and password
/// when credentials are configured and no authentication otherwise. Offering
/// only one keeps a proxy from downgrading a configured login to none.
pub fn greeting(with_auth: bool) -> [u8; 3] {
    let method = if with_auth {
        METHOD_USERPASS
    } else {
        METHOD_NONE
    };
    [VERSION, 1, method]
}

/// Parses the proxy's method choice (two bytes).
pub fn parse_method(reply: [u8; 2], with_auth: bool) -> Result<Method, SocksError> {
    if reply[0] != VERSION {
        return Err(SocksError::Malformed("wrong version in the method choice"));
    }
    match (reply[1], with_auth) {
        (METHOD_NONE, false) => Ok(Method::NoAuth),
        (METHOD_USERPASS, true) => Ok(Method::UserPass),
        _ => Err(SocksError::NoAcceptableMethod),
    }
}

/// The RFC 1929 request. Each field must be 1 to 255 bytes.
pub fn auth_request(username: &[u8], password: &[u8]) -> Result<Vec<u8>, SocksError> {
    let ulen = field_len(username)?;
    let plen = field_len(password)?;
    let mut out = Vec::with_capacity(3 + username.len() + password.len());
    out.push(AUTH_VERSION);
    out.push(ulen);
    out.extend_from_slice(username);
    out.push(plen);
    out.extend_from_slice(password);
    Ok(out)
}

fn field_len(field: &[u8]) -> Result<u8, SocksError> {
    match u8::try_from(field.len()) {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(SocksError::TooLong),
    }
}

/// Parses the RFC 1929 reply (two bytes). Any status but 0 is a refusal.
/// RFC 1929 says the version byte is 1; some proxies echo 5, which is
/// accepted, since the status byte is what matters.
pub fn parse_auth(reply: [u8; 2]) -> Result<(), SocksError> {
    if reply[0] != AUTH_VERSION && reply[0] != VERSION {
        return Err(SocksError::Malformed("wrong version in the login reply"));
    }
    if reply[1] == 0 {
        Ok(())
    } else {
        Err(SocksError::AuthRejected)
    }
}

/// The `CONNECT` request for `target`.
pub fn connect_request(target: &Target) -> Result<Vec<u8>, SocksError> {
    let mut out = Vec::with_capacity(22);
    out.extend_from_slice(&[VERSION, CMD_CONNECT, 0]);
    let port = match target {
        Target::Addr(SocketAddr::V4(a)) => {
            out.push(ATYP_V4);
            out.extend_from_slice(&a.ip().octets());
            a.port()
        }
        Target::Addr(SocketAddr::V6(a)) => {
            out.push(ATYP_V6);
            out.extend_from_slice(&a.ip().octets());
            a.port()
        }
        Target::Name(name, port) => {
            let len = field_len(name.as_bytes())?;
            out.push(ATYP_DOMAIN);
            out.push(len);
            out.extend_from_slice(name.as_bytes());
            *port
        }
    };
    out.extend_from_slice(&port.to_be_bytes());
    Ok(out)
}

/// The full length of a `CONNECT` reply from its first [`REPLY_PREFIX`]
/// bytes, checking the head on the way, so the caller can read exactly
/// that many bytes. A failure code is reported here, before the rest is
/// read, since the address of a failed reply is meaningless.
pub fn reply_len(prefix: [u8; REPLY_PREFIX]) -> Result<usize, SocksError> {
    let [ver, rep, rsv, atyp, first] = prefix;
    if ver != VERSION {
        return Err(SocksError::Malformed("wrong version in the reply"));
    }
    if rep != 0 {
        return Err(SocksError::Refused(rep));
    }
    if rsv != 0 {
        return Err(SocksError::Malformed("the reserved byte isn't zero"));
    }
    match atyp {
        ATYP_V4 => Ok(4 + 4 + 2),
        ATYP_V6 => Ok(4 + 16 + 2),
        ATYP_DOMAIN if first == 0 => Err(SocksError::Malformed("an empty name")),
        ATYP_DOMAIN => Ok(4 + 1 + usize::from(first) + 2),
        _ => Err(SocksError::Malformed("an unknown address type")),
    }
}

/// Parses a whole `CONNECT` reply: exactly the bytes [`reply_len`] asked
/// for, no more and no fewer.
pub fn parse_reply(reply: &[u8]) -> Result<Bound, SocksError> {
    let Some(prefix) = reply.first_chunk::<REPLY_PREFIX>() else {
        return Err(SocksError::Malformed("the reply is too short"));
    };
    let len = reply_len(*prefix)?;
    if reply.len() != len {
        return Err(SocksError::Malformed("the reply has the wrong length"));
    }
    let (addr, port) = reply[4..].split_at(len - 6);
    let port = u16::from_be_bytes([port[0], port[1]]);
    match reply[3] {
        ATYP_V4 => {
            let o: [u8; 4] = addr
                .try_into()
                .map_err(|_| SocksError::Malformed("a short address"))?;
            Ok(Bound::Addr(SocketAddr::new(Ipv4Addr::from(o).into(), port)))
        }
        ATYP_V6 => {
            let o: [u8; 16] = addr
                .try_into()
                .map_err(|_| SocksError::Malformed("a short address"))?;
            Ok(Bound::Addr(SocketAddr::new(Ipv6Addr::from(o).into(), port)))
        }
        _ => {
            let name = &addr[1..];
            if !name.iter().all(|b| b.is_ascii_graphic()) {
                return Err(SocksError::Malformed("the name isn't printable ASCII"));
            }
            // All bytes are ASCII, so this can't fail.
            let name = std::str::from_utf8(name)
                .map_err(|_| SocksError::Malformed("the name isn't printable ASCII"))?;
            Ok(Bound::Name(name.to_owned(), port))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn encode_reply(bound: &Bound) -> Vec<u8> {
        let mut out = vec![VERSION, 0, 0];
        let port = match bound {
            Bound::Addr(SocketAddr::V4(a)) => {
                out.push(ATYP_V4);
                out.extend_from_slice(&a.ip().octets());
                a.port()
            }
            Bound::Addr(SocketAddr::V6(a)) => {
                out.push(ATYP_V6);
                out.extend_from_slice(&a.ip().octets());
                a.port()
            }
            Bound::Name(n, p) => {
                out.push(ATYP_DOMAIN);
                out.push(n.len() as u8);
                out.extend_from_slice(n.as_bytes());
                *p
            }
        };
        out.extend_from_slice(&port.to_be_bytes());
        out
    }

    #[test]
    fn greeting_offers_exactly_one_method() {
        assert_eq!(greeting(false), [5, 1, 0]);
        assert_eq!(greeting(true), [5, 1, 2]);
    }

    #[test]
    fn the_proxy_must_pick_the_offered_method() {
        assert_eq!(parse_method([5, 0], false), Ok(Method::NoAuth));
        assert_eq!(parse_method([5, 2], true), Ok(Method::UserPass));
        // A login was configured; "no authentication" is a downgrade.
        assert_eq!(
            parse_method([5, 0], true),
            Err(SocksError::NoAcceptableMethod)
        );
        assert_eq!(
            parse_method([5, 2], false),
            Err(SocksError::NoAcceptableMethod)
        );
        assert_eq!(
            parse_method([5, 0xff], true),
            Err(SocksError::NoAcceptableMethod)
        );
        assert!(matches!(
            parse_method([4, 0], false),
            Err(SocksError::Malformed(_))
        ));
    }

    #[test]
    fn auth_fields_are_bounded() {
        assert_eq!(
            auth_request(b"u", b"pw").unwrap(),
            vec![1, 1, b'u', 2, b'p', b'w']
        );
        assert_eq!(auth_request(b"", b"pw"), Err(SocksError::TooLong));
        assert_eq!(auth_request(b"u", &[b'x'; 256]), Err(SocksError::TooLong));
        assert!(auth_request(&[b'x'; 255], &[b'y'; 255]).is_ok());
        assert_eq!(parse_auth([1, 0]), Ok(()));
        assert_eq!(parse_auth([5, 0]), Ok(()));
        assert_eq!(parse_auth([1, 1]), Err(SocksError::AuthRejected));
        assert!(matches!(parse_auth([9, 0]), Err(SocksError::Malformed(_))));
    }

    #[test]
    fn connect_requests_encode_each_target() {
        let v4: SocketAddr = "1.2.3.4:6697".parse().unwrap();
        assert_eq!(
            connect_request(&Target::Addr(v4)).unwrap(),
            vec![5, 1, 0, 1, 1, 2, 3, 4, 0x1a, 0x29]
        );
        let v6: SocketAddr = "[2001:4860::1]:443".parse().unwrap();
        let req = connect_request(&Target::Addr(v6)).unwrap();
        assert_eq!(req.len(), 22);
        assert_eq!(req[3], ATYP_V6);
        let req = connect_request(&Target::Name("irc.example".into(), 6697)).unwrap();
        assert_eq!(&req[..5], &[5, 1, 0, 3, 11]);
        assert_eq!(&req[5..16], b"irc.example");
        assert_eq!(
            connect_request(&Target::Name(String::new(), 1)),
            Err(SocksError::TooLong)
        );
        assert_eq!(
            connect_request(&Target::Name("a".repeat(256), 1)),
            Err(SocksError::TooLong)
        );
    }

    #[test]
    fn malformed_replies_are_refused() {
        let bad: &[(&[u8], &str)] = &[
            (&[4, 0, 0, 1, 0, 0, 0, 0, 0, 0], "version"),
            (&[5, 0, 1, 1, 0, 0, 0, 0, 0, 0], "reserved"),
            (&[5, 0, 0, 2, 0, 0, 0, 0, 0, 0], "address type"),
            (&[5, 0, 0, 3, 0, 0, 0], "empty name"),
            (&[5, 0, 0, 3, 2, b'a', 0x0a, 0, 0], "control byte"),
            (&[5, 0, 0, 1, 0, 0, 0, 0, 0], "short"),
            (&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0], "long"),
            (&[5, 0, 0], "truncated head"),
        ];
        for (bytes, why) in bad {
            assert!(
                matches!(parse_reply(bytes), Err(SocksError::Malformed(_))),
                "{why}"
            );
        }
        assert_eq!(
            parse_reply(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]),
            Err(SocksError::Refused(5))
        );
    }

    #[test]
    fn failure_codes_are_named_without_proxy_bytes() {
        let msg = SocksError::Refused(0x05).to_string();
        assert!(msg.contains("connection refused"), "{msg}");
        assert!(SocksError::Refused(0x99).to_string().contains("unassigned"));
    }

    proptest! {
        #[test]
        fn arbitrary_replies_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            match parse_reply(&bytes) {
                Ok(Bound::Name(n, _)) => {
                    prop_assert!(!n.is_empty() && n.len() <= 255);
                    prop_assert!(n.bytes().all(|b| b.is_ascii_graphic()));
                    prop_assert!(bytes.len() <= MAX_REPLY);
                }
                Ok(Bound::Addr(_)) => prop_assert!(bytes.len() == 10 || bytes.len() == 22),
                Err(_) => {}
            }
            if let Some(p) = bytes.first_chunk::<REPLY_PREFIX>()
                && let Ok(n) = reply_len(*p)
            {
                prop_assert!((10..=MAX_REPLY).contains(&n));
            }
        }

        #[test]
        fn encoded_replies_round_trip(
            v4 in any::<[u8; 4]>(),
            v6 in any::<[u8; 16]>(),
            name in "[a-z0-9.-]{1,255}",
            port in any::<u16>(),
        ) {
            for bound in [
                Bound::Addr(SocketAddr::new(Ipv4Addr::from(v4).into(), port)),
                Bound::Addr(SocketAddr::new(Ipv6Addr::from(v6).into(), port)),
                Bound::Name(name.clone(), port),
            ] {
                let bytes = encode_reply(&bound);
                let prefix = bytes.first_chunk::<REPLY_PREFIX>().unwrap();
                prop_assert_eq!(reply_len(*prefix), Ok(bytes.len()));
                prop_assert_eq!(parse_reply(&bytes), Ok(bound));
            }
        }
    }
}
