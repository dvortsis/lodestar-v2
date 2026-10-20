//! KRPC messages (BEP 5) with the extensions the crawler speaks: IPv6 nodes
//! (BEP 32), DHT scrape (BEP 33), external-IP reporting for secure ids
//! (BEP 42), read-only nodes (BEP 43) and `sample_infohashes` (BEP 51).
//!
//! Every packet is untrusted. Parsing is zero-copy, bounded by tight
//! bencode limits, and strict where a mistake would mislead the node (a
//! missing `id`, `nodes` that aren't whole entries, `samples` that aren't
//! whole hashes). Optional extras that are malformed (`ip`, `v`, a bad
//! `values` entry, a wrong-sized bloom filter) are ignored rather than
//! fatal, because real clients vary. Unknown keys are ignored and unknown
//! query methods are surfaced as [`Method::Unknown`] so the node can answer
//! with error 204.
//!
//! Encoding is canonical bencode, so `parse(encode(m)) == m`.

use std::fmt;
use std::net::SocketAddr;

use lodestar_bencode::{
    self as bencode, DecodeError, DecodeOptions, Dict, DictEncoder, Encoder, Value,
};
use lodestar_metainfo::InfoHash;

use crate::compact::{self, CompactNodes, NodeId};

/// Bencode limits for one KRPC packet: message → `a`/`r` → `values` →
/// entries is the deepest legitimate nesting.
const LIMITS: bencode::Limits = bencode::Limits {
    max_depth: 4,
    max_items: 4096,
};

/// Longest transaction id accepted; real clients use 2–8 bytes.
pub const MAX_TRANSACTION_ID: usize = 32;

/// Standard KRPC error codes.
pub mod error_code {
    pub const GENERIC: i64 = 201;
    pub const SERVER: i64 = 202;
    pub const PROTOCOL: i64 = 203;
    pub const METHOD_UNKNOWN: i64 = 204;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message<'a> {
    pub transaction_id: &'a [u8],
    /// Client version (`v`), e.g. `LT\x01\x02`.
    pub version: Option<&'a [u8]>,
    /// The sender's view of our address (BEP 42 `ip`).
    pub ip: Option<SocketAddr>,
    /// The sender is read-only and must not be added to routing tables
    /// (BEP 43 `ro`).
    pub read_only: bool,
    pub body: Body<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body<'a> {
    Query(Query<'a>),
    Response(Response<'a>),
    Error(ErrorBody<'a>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query<'a> {
    pub id: NodeId,
    pub method: Method<'a>,
}

/// Which address families the querier wants nodes for (BEP 32 `want`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Want {
    pub v4: bool,
    pub v6: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method<'a> {
    Ping,
    FindNode {
        target: NodeId,
        want: Want,
    },
    GetPeers {
        info_hash: InfoHash,
        want: Want,
        /// BEP 33: also return seed/peer bloom filters.
        scrape: bool,
        /// BEP 33: don't return seeds in `values`.
        noseed: bool,
    },
    AnnouncePeer {
        info_hash: InfoHash,
        port: u16,
        /// Use the packet's source port instead of `port`.
        implied_port: bool,
        token: &'a [u8],
        seed: bool,
    },
    /// BEP 51.
    SampleInfohashes {
        target: NodeId,
        want: Want,
    },
    Unknown(&'a [u8]),
}

impl Method<'_> {
    pub fn name(&self) -> &[u8] {
        match self {
            Method::Ping => b"ping",
            Method::FindNode { .. } => b"find_node",
            Method::GetPeers { .. } => b"get_peers",
            Method::AnnouncePeer { .. } => b"announce_peer",
            Method::SampleInfohashes { .. } => b"sample_infohashes",
            Method::Unknown(name) => name,
        }
    }
}

/// A response. KRPC responses don't name their method (they're matched
/// by transaction id), so every optional field any method uses is here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Response<'a> {
    pub id: NodeId,
    pub nodes: CompactNodes<'a>,
    pub nodes6: CompactNodes<'a>,
    /// Peers from `get_peers`.
    pub values: Vec<SocketAddr>,
    pub token: Option<&'a [u8]>,
    /// BEP 51 `samples`: concatenated 20-byte info hashes.
    pub samples: Samples<'a>,
    /// BEP 51: seconds before this node should be sampled again.
    pub interval: Option<u32>,
    /// BEP 51: how many info hashes the node holds.
    pub num: Option<u32>,
    /// BEP 33 `BFsd` (seeds) and `BFpe` (peers) bloom filters.
    pub seeds_bloom: Option<&'a [u8; 256]>,
    pub peers_bloom: Option<&'a [u8; 256]>,
}

/// Concatenated 20-byte info hashes.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Samples<'a>(&'a [u8]);

impl<'a> Samples<'a> {
    pub fn new(raw: &'a [u8]) -> Option<Samples<'a>> {
        (raw.len() % 20 == 0).then_some(Samples(raw))
    }

    pub fn raw(&self) -> &'a [u8] {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len() / 20
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = InfoHash> + 'a {
        self.0
            .chunks_exact(20)
            .map(|c| InfoHash(c.try_into().expect("20-byte chunk")))
    }
}

impl fmt::Debug for Samples<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorBody<'a> {
    pub code: i64,
    pub message: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    Bencode(DecodeError),
    NotADict,
    Missing(&'static str),
    Invalid(&'static str),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Bencode(e) => write!(f, "krpc: {e}"),
            ParseError::NotADict => f.write_str("krpc: message is not a dictionary"),
            ParseError::Missing(k) => write!(f, "krpc: missing {k:?}"),
            ParseError::Invalid(k) => write!(f, "krpc: invalid {k:?}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<DecodeError> for ParseError {
    fn from(e: DecodeError) -> Self {
        ParseError::Bencode(e)
    }
}

/// Parses one KRPC packet.
pub fn parse(packet: &[u8]) -> Result<Message<'_>, ParseError> {
    let value = bencode::decode_with(packet, &DecodeOptions::lenient().with_limits(LIMITS))?;
    let top = value.as_dict().ok_or(ParseError::NotADict)?;
    let transaction_id = bytes(top, "t")?.ok_or(ParseError::Missing("t"))?;
    if transaction_id.len() > MAX_TRANSACTION_ID {
        return Err(ParseError::Invalid("t"));
    }
    let body = match bytes(top, "y")?.ok_or(ParseError::Missing("y"))? {
        b"q" => Body::Query(parse_query(top)?),
        b"r" => Body::Response(parse_response(top)?),
        b"e" => Body::Error(parse_error(top)?),
        _ => return Err(ParseError::Invalid("y")),
    };
    Ok(Message {
        transaction_id,
        version: top.get(b"v").and_then(Value::as_bytes),
        ip: top
            .get(b"ip")
            .and_then(Value::as_bytes)
            .and_then(compact::decode_addr),
        read_only: top.get(b"ro").and_then(Value::as_int) == Some(1),
        body,
    })
}

fn parse_query<'a>(top: &Dict<'a>) -> Result<Query<'a>, ParseError> {
    let name = bytes(top, "q")?.ok_or(ParseError::Missing("q"))?;
    let args = top
        .get(b"a")
        .ok_or(ParseError::Missing("a"))?
        .as_dict()
        .ok_or(ParseError::Invalid("a"))?;
    let id = id20(args, "id")?;
    let method = match name {
        b"ping" => Method::Ping,
        b"find_node" => Method::FindNode {
            target: id20(args, "target")?,
            want: want(args),
        },
        b"get_peers" => Method::GetPeers {
            info_hash: InfoHash(id20(args, "info_hash")?.0),
            want: want(args),
            scrape: flag(args, "scrape"),
            noseed: flag(args, "noseed"),
        },
        b"announce_peer" => {
            let implied_port = flag(args, "implied_port");
            let port = match args.get(b"port").and_then(Value::as_int) {
                Some(p) => u16::try_from(p).map_err(|_| ParseError::Invalid("port"))?,
                None if implied_port => 0,
                None => return Err(ParseError::Missing("port")),
            };
            Method::AnnouncePeer {
                info_hash: InfoHash(id20(args, "info_hash")?.0),
                port,
                implied_port,
                token: bytes(args, "token")?.ok_or(ParseError::Missing("token"))?,
                seed: flag(args, "seed"),
            }
        }
        b"sample_infohashes" => Method::SampleInfohashes {
            target: id20(args, "target")?,
            want: want(args),
        },
        other => Method::Unknown(other),
    };
    Ok(Query { id, method })
}

fn parse_response<'a>(top: &Dict<'a>) -> Result<Response<'a>, ParseError> {
    let r = top
        .get(b"r")
        .ok_or(ParseError::Missing("r"))?
        .as_dict()
        .ok_or(ParseError::Invalid("r"))?;
    let nodes = match bytes(r, "nodes")? {
        Some(raw) if !raw.is_empty() => {
            CompactNodes::v4(raw).ok_or(ParseError::Invalid("nodes"))?
        }
        _ => CompactNodes::default(),
    };
    let nodes6 = match bytes(r, "nodes6")? {
        Some(raw) if !raw.is_empty() => {
            CompactNodes::v6(raw).ok_or(ParseError::Invalid("nodes6"))?
        }
        _ => CompactNodes::default(),
    };
    let values = match r.get(b"values") {
        Some(v) => v
            .as_list()
            .ok_or(ParseError::Invalid("values"))?
            .iter()
            .filter_map(|p| p.as_bytes().and_then(compact::decode_addr))
            .collect(),
        None => Vec::new(),
    };
    let samples = match bytes(r, "samples")? {
        Some(raw) => Samples::new(raw).ok_or(ParseError::Invalid("samples"))?,
        None => Samples::default(),
    };
    let u32_field = |key: &str| {
        r.get(key.as_bytes())
            .and_then(Value::as_int)
            .and_then(|v| u32::try_from(v).ok())
    };
    let bloom = |key: &str| {
        r.get(key.as_bytes())
            .and_then(Value::as_bytes)
            .and_then(|b| b.try_into().ok())
    };
    Ok(Response {
        id: id20(r, "id")?,
        nodes,
        nodes6,
        values,
        token: bytes(r, "token")?,
        samples,
        interval: u32_field("interval"),
        num: u32_field("num"),
        seeds_bloom: bloom("BFsd"),
        peers_bloom: bloom("BFpe"),
    })
}

fn parse_error<'a>(top: &Dict<'a>) -> Result<ErrorBody<'a>, ParseError> {
    let e = top
        .get(b"e")
        .ok_or(ParseError::Missing("e"))?
        .as_list()
        .ok_or(ParseError::Invalid("e"))?;
    Ok(ErrorBody {
        code: e
            .get(0)
            .and_then(Value::as_int)
            .ok_or(ParseError::Invalid("e"))?,
        message: e.get(1).and_then(Value::as_bytes).unwrap_or_default(),
    })
}

fn bytes<'a>(d: &Dict<'a>, key: &'static str) -> Result<Option<&'a [u8]>, ParseError> {
    match d.get(key.as_bytes()) {
        None => Ok(None),
        Some(v) => v.as_bytes().map(Some).ok_or(ParseError::Invalid(key)),
    }
}

fn id20(d: &Dict<'_>, key: &'static str) -> Result<NodeId, ParseError> {
    let b = bytes(d, key)?.ok_or(ParseError::Missing(key))?;
    NodeId::from_slice(b).ok_or(ParseError::Invalid(key))
}

fn flag(d: &Dict<'_>, key: &str) -> bool {
    d.get(key.as_bytes()).and_then(Value::as_int) == Some(1)
}

fn want(d: &Dict<'_>) -> Want {
    let mut w = Want::default();
    if let Some(list) = d.get(b"want").and_then(Value::as_list) {
        for item in list.iter().filter_map(Value::as_bytes) {
            match item {
                b"n4" => w.v4 = true,
                b"n6" => w.v6 = true,
                _ => {}
            }
        }
    }
    w
}

impl Message<'_> {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        self.encode_into(&mut out);
        out
    }

    /// Appends the canonical encoding. Keys are written in sorted order;
    /// the encoder panics on a mistake, which the tests exercise.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        Encoder::new(out).dict(|d| {
            if let Body::Query(q) = &self.body {
                encode_args(d.entry(b"a"), q);
            }
            if let Body::Error(e) = &self.body {
                d.entry(b"e").list(|l| {
                    l.item().int(e.code);
                    l.item().bytes(e.message);
                });
            }
            if let Some(ip) = &self.ip {
                let mut b = Vec::with_capacity(18);
                compact::encode_addr(ip, &mut b);
                d.entry(b"ip").bytes(&b);
            }
            if let Body::Query(q) = &self.body {
                d.entry(b"q").bytes(q.method.name());
            }
            if let Body::Response(r) = &self.body {
                encode_response(d.entry(b"r"), r);
            }
            if self.read_only {
                d.entry(b"ro").int(1);
            }
            d.entry(b"t").bytes(self.transaction_id);
            if let Some(v) = self.version {
                d.entry(b"v").bytes(v);
            }
            d.entry(b"y").bytes(match self.body {
                Body::Query(_) => b"q",
                Body::Response(_) => b"r",
                Body::Error(_) => b"e",
            });
        });
    }
}

fn encode_want(d: &mut DictEncoder<'_>, want: Want) {
    if want.v4 || want.v6 {
        d.entry(b"want").list(|l| {
            if want.v4 {
                l.item().bytes(b"n4");
            }
            if want.v6 {
                l.item().bytes(b"n6");
            }
        });
    }
}

fn encode_args(enc: Encoder<'_>, q: &Query<'_>) {
    enc.dict(|d| {
        d.entry(b"id").bytes(&q.id.0);
        match &q.method {
            Method::Ping | Method::Unknown(_) => {}
            Method::FindNode { target, want } | Method::SampleInfohashes { target, want } => {
                d.entry(b"target").bytes(&target.0);
                encode_want(d, *want);
            }
            Method::GetPeers {
                info_hash,
                want,
                scrape,
                noseed,
            } => {
                d.entry(b"info_hash").bytes(&info_hash.0);
                if *noseed {
                    d.entry(b"noseed").int(1);
                }
                if *scrape {
                    d.entry(b"scrape").int(1);
                }
                encode_want(d, *want);
            }
            Method::AnnouncePeer {
                info_hash,
                port,
                implied_port,
                token,
                seed,
            } => {
                if *implied_port {
                    d.entry(b"implied_port").int(1);
                }
                d.entry(b"info_hash").bytes(&info_hash.0);
                d.entry(b"port").int(i64::from(*port));
                if *seed {
                    d.entry(b"seed").int(1);
                }
                d.entry(b"token").bytes(token);
            }
        }
    });
}

fn encode_response(enc: Encoder<'_>, r: &Response<'_>) {
    enc.dict(|d| {
        if let Some(bf) = r.peers_bloom {
            d.entry(b"BFpe").bytes(bf);
        }
        if let Some(bf) = r.seeds_bloom {
            d.entry(b"BFsd").bytes(bf);
        }
        d.entry(b"id").bytes(&r.id.0);
        if let Some(i) = r.interval {
            d.entry(b"interval").int(i64::from(i));
        }
        if !r.nodes.is_empty() {
            d.entry(b"nodes").bytes(r.nodes.raw());
        }
        if !r.nodes6.is_empty() {
            d.entry(b"nodes6").bytes(r.nodes6.raw());
        }
        if let Some(n) = r.num {
            d.entry(b"num").int(i64::from(n));
        }
        if !r.samples.is_empty() {
            d.entry(b"samples").bytes(r.samples.raw());
        }
        if let Some(t) = r.token {
            d.entry(b"token").bytes(t);
        }
        if !r.values.is_empty() {
            d.entry(b"values").list(|l| {
                for v in &r.values {
                    let mut b = Vec::with_capacity(18);
                    compact::encode_addr(v, &mut b);
                    l.item().bytes(&b);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &[u8; 20] = b"abcdefghij0123456789";

    #[test]
    fn bep5_examples() {
        // The examples from BEP 5, byte for byte.
        let ping = b"d1:ad2:id20:abcdefghij0123456789e1:q4:ping1:t2:aa1:y1:qe";
        let m = parse(ping).unwrap();
        assert_eq!(m.transaction_id, b"aa");
        assert_eq!(
            m.body,
            Body::Query(Query {
                id: NodeId(*ID),
                method: Method::Ping
            })
        );
        assert_eq!(m.encode(), ping);

        let pong = b"d1:rd2:id20:mnopqrstuvwxyz123456e1:t2:aa1:y1:re";
        let m = parse(pong).unwrap();
        let Body::Response(r) = &m.body else { panic!() };
        assert_eq!(r.id.0, *b"mnopqrstuvwxyz123456");
        assert_eq!(m.encode(), pong);

        let err = b"d1:eli201e23:A Generic Error Ocurrede1:t2:aa1:y1:ee";
        let m = parse(err).unwrap();
        assert_eq!(
            m.body,
            Body::Error(ErrorBody {
                code: 201,
                message: b"A Generic Error Ocurred"
            })
        );
        assert_eq!(m.encode(), err);

        let announce = b"d1:ad2:id20:abcdefghij012345678912:implied_porti1e9:info_hash20:mnopqrstuvwxyz1234564:porti6881e5:token8:aoeusnthe1:q13:announce_peer1:t2:aa1:y1:qe";
        let m = parse(announce).unwrap();
        let Body::Query(q) = &m.body else { panic!() };
        assert_eq!(
            q.method,
            Method::AnnouncePeer {
                info_hash: InfoHash(*b"mnopqrstuvwxyz123456"),
                port: 6881,
                implied_port: true,
                token: b"aoeusnth",
                seed: false,
            }
        );
        assert_eq!(m.encode(), &announce[..]);
    }

    #[test]
    fn get_peers_response_with_values_and_scrape() {
        let peer: SocketAddr = "1.2.3.4:5678".parse().unwrap();
        let peer6: SocketAddr = "[2001:db8::2]:80".parse().unwrap();
        let bf = [0xa5u8; 256];
        let msg = Message {
            transaction_id: b"xy",
            version: Some(b"LS01"),
            ip: Some("9.8.7.6:1".parse().unwrap()),
            read_only: false,
            body: Body::Response(Response {
                id: NodeId([7; 20]),
                values: vec![peer, peer6],
                token: Some(b"tok"),
                seeds_bloom: Some(&bf),
                peers_bloom: Some(&bf),
                ..Default::default()
            }),
        };
        let bytes = msg.encode();
        assert_eq!(parse(&bytes).unwrap(), msg);
    }

    #[test]
    fn sample_infohashes_roundtrip() {
        let samples = [[1u8; 20], [2u8; 20]].concat();
        let nodes = compact::encode_nodes(
            &[crate::NodeInfo {
                id: NodeId([3; 20]),
                addr: "10.0.0.1:6881".parse().unwrap(),
            }],
            false,
        );
        let msg = Message {
            transaction_id: b"s1",
            version: None,
            ip: None,
            read_only: true,
            body: Body::Response(Response {
                id: NodeId([4; 20]),
                nodes: CompactNodes::v4(&nodes).unwrap(),
                samples: Samples::new(&samples).unwrap(),
                interval: Some(21_600),
                num: Some(1234),
                ..Default::default()
            }),
        };
        let bytes = msg.encode();
        let back = parse(&bytes).unwrap();
        assert_eq!(back, msg);
        let Body::Response(r) = back.body else {
            panic!()
        };
        assert_eq!(
            r.samples.iter().collect::<Vec<_>>(),
            [InfoHash([1; 20]), InfoHash([2; 20])]
        );
        assert_eq!(r.nodes.len(), 1);
    }

    #[test]
    fn unknown_methods_and_lenient_extras() {
        let m = parse(b"d1:ad2:id20:abcdefghij0123456789e1:q4:vote1:t1:x2:ip3:bad1:v0:1:y1:qe")
            .unwrap();
        let Body::Query(q) = &m.body else { panic!() };
        assert_eq!(q.method, Method::Unknown(b"vote"));
        assert_eq!(m.ip, None, "malformed ip is ignored");
        assert_eq!(m.version, Some(&b""[..]));
    }

    #[test]
    fn rejections() {
        let cases: &[(&[u8], ParseError)] = &[
            (b"le", ParseError::NotADict),
            (b"d1:y1:qe", ParseError::Missing("t")),
            (b"d1:t1:x1:y1:ze", ParseError::Invalid("y")),
            (b"d1:t1:x1:y1:qe", ParseError::Missing("q")),
            (b"d1:ad2:id3:abce1:q4:ping1:t1:x1:y1:qe", ParseError::Invalid("id")),
            (b"d1:ade1:q4:ping1:t1:x1:y1:qe", ParseError::Missing("id")),
            (
                b"d1:ad2:id20:abcdefghij0123456789e1:q9:find_node1:t1:x1:y1:qe",
                ParseError::Missing("target"),
            ),
            (
                b"d1:rd2:id20:abcdefghij01234567895:nodes3:abce1:t1:x1:y1:re",
                ParseError::Invalid("nodes"),
            ),
            (
                b"d1:rd2:id20:abcdefghij01234567897:samples3:abce1:t1:x1:y1:re",
                ParseError::Invalid("samples"),
            ),
            (
                b"d1:ad2:id20:abcdefghij01234567899:info_hash20:abcdefghij01234567894:porti70000e5:token1:xe1:q13:announce_peer1:t1:x1:y1:qe",
                ParseError::Invalid("port"),
            ),
            (b"d1:eli201ee1:t33:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1:y1:ee", ParseError::Invalid("t")),
        ];
        for (input, expected) in cases {
            assert_eq!(
                parse(input).unwrap_err(),
                *expected,
                "{}",
                input.escape_ascii()
            );
        }
    }

    #[test]
    fn nesting_beyond_krpc_shape_is_rejected() {
        let deep = b"d1:rd2:id20:abcdefghij01234567896:valuesllleeee1:t1:x1:y1:re";
        assert!(matches!(parse(deep), Err(ParseError::Bencode(_))));
    }
}
