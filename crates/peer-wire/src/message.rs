//! Message bodies: the BEP 3 handshake, the BEP 10 extended handshake,
//! BEP 9 `ut_metadata` and BEP 11 `ut_pex`.

use std::net::SocketAddr;

use lodestar_bencode::{self as bencode, DecodeOptions, Encoder, Value};

use crate::Error;

pub const PROTOCOL: &[u8; 19] = b"BitTorrent protocol";
pub const HANDSHAKE_LEN: usize = 68;
/// BEP 9 metadata pieces are 16 KiB (the last one may be shorter).
pub const METADATA_PIECE: usize = 16 * 1024;
/// Our extended message ids, as advertised in our extended handshake.
pub const OUR_UT_METADATA: u8 = 1;
pub const OUR_UT_PEX: u8 = 2;

/// Bencode limits for extension dictionaries.
const LIMITS: bencode::Limits = bencode::Limits {
    max_depth: 4,
    max_items: 4096,
};

pub fn handshake(
    info_hash: &[u8; 20],
    peer_id: &[u8; 20],
    extensions: bool,
) -> [u8; HANDSHAKE_LEN] {
    let mut out = [0u8; HANDSHAKE_LEN];
    out[0] = 19;
    out[1..20].copy_from_slice(PROTOCOL);
    if extensions {
        out[25] |= 0x10; // BEP 10
    }
    out[28..48].copy_from_slice(info_hash);
    out[48..68].copy_from_slice(peer_id);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    pub extensions: bool,
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

pub fn parse_handshake(b: &[u8; HANDSHAKE_LEN]) -> Result<Handshake, Error> {
    if b[0] != 19 || &b[1..20] != PROTOCOL {
        return Err(Error::Handshake("not a BitTorrent handshake"));
    }
    Ok(Handshake {
        extensions: b[25] & 0x10 != 0,
        info_hash: b[28..48].try_into().expect("20 bytes"),
        peer_id: b[48..68].try_into().expect("20 bytes"),
    })
}

/// Our extended handshake: which extension ids we use.
pub fn our_extended_handshake(metadata_size: Option<usize>) -> Vec<u8> {
    let mut out = Vec::new();
    Encoder::new(&mut out).dict(|d| {
        d.entry(b"m").dict(|m| {
            m.entry(b"ut_metadata").int(i64::from(OUR_UT_METADATA));
            m.entry(b"ut_pex").int(i64::from(OUR_UT_PEX));
        });
        if let Some(size) = metadata_size {
            d.entry(b"metadata_size").int(size as i64);
        }
        d.entry(b"v").str("Lodestar");
    });
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtendedHandshake {
    /// The id the peer wants for `ut_metadata` messages we send it.
    pub ut_metadata: Option<u8>,
    pub ut_pex: Option<u8>,
    pub metadata_size: Option<i64>,
    /// The peer's client name (`v`).
    pub client: Option<String>,
}

pub fn parse_extended_handshake(payload: &[u8]) -> Result<ExtendedHandshake, Error> {
    let v = bencode::decode_with(payload, &DecodeOptions::lenient().with_limits(LIMITS))
        .map_err(|_| Error::Protocol("extended handshake is not bencode"))?;
    let d = v
        .as_dict()
        .ok_or(Error::Protocol("extended handshake is not a dictionary"))?;
    let ext_id = |name: &[u8]| {
        d.get(b"m")
            .and_then(Value::as_dict)
            .and_then(|m| m.get(name))
            .and_then(Value::as_int)
            .and_then(|id| u8::try_from(id).ok())
            .filter(|id| *id != 0) // 0 means "disabled"
    };
    Ok(ExtendedHandshake {
        ut_metadata: ext_id(b"ut_metadata"),
        ut_pex: ext_id(b"ut_pex"),
        metadata_size: d.get(b"metadata_size").and_then(Value::as_int),
        client: d
            .get(b"v")
            .and_then(Value::as_bytes)
            .map(|b| String::from_utf8_lossy(&b[..b.len().min(64)]).into_owned()),
    })
}

pub fn metadata_request(piece: usize) -> Vec<u8> {
    let mut out = Vec::new();
    Encoder::new(&mut out).dict(|d| {
        d.entry(b"msg_type").int(0);
        d.entry(b"piece").int(piece as i64);
    });
    out
}

pub fn metadata_reject(piece: usize) -> Vec<u8> {
    let mut out = Vec::new();
    Encoder::new(&mut out).dict(|d| {
        d.entry(b"msg_type").int(2);
        d.entry(b"piece").int(piece as i64);
    });
    out
}

pub fn metadata_data(piece: usize, total: usize, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    Encoder::new(&mut out).dict(|d| {
        d.entry(b"msg_type").int(1);
        d.entry(b"piece").int(piece as i64);
        d.entry(b"total_size").int(total as i64);
    });
    out.extend_from_slice(data);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataMessage<'a> {
    Request {
        piece: i64,
    },
    Data {
        piece: i64,
        total_size: Option<i64>,
        data: &'a [u8],
    },
    Reject {
        piece: i64,
    },
}

pub fn parse_metadata_message(payload: &[u8]) -> Result<MetadataMessage<'_>, Error> {
    let (v, used) = bencode::decode_prefix(payload, &DecodeOptions::lenient().with_limits(LIMITS))
        .map_err(|_| Error::Protocol("ut_metadata header is not bencode"))?;
    let d = v
        .as_dict()
        .ok_or(Error::Protocol("ut_metadata header is not a dictionary"))?;
    let int = |k: &[u8]| d.get(k).and_then(Value::as_int);
    let piece = int(b"piece").ok_or(Error::Protocol("ut_metadata without piece"))?;
    match int(b"msg_type") {
        Some(0) => Ok(MetadataMessage::Request { piece }),
        Some(1) => Ok(MetadataMessage::Data {
            piece,
            total_size: int(b"total_size"),
            data: &payload[used..],
        }),
        Some(2) => Ok(MetadataMessage::Reject { piece }),
        _ => Err(Error::Protocol("unknown ut_metadata msg_type")),
    }
}

/// Peers from a `ut_pex` message (`added` and `added6`).
pub fn parse_pex(payload: &[u8]) -> Vec<SocketAddr> {
    let Ok(v) = bencode::decode_with(payload, &DecodeOptions::lenient().with_limits(LIMITS)) else {
        return Vec::new();
    };
    let Some(d) = v.as_dict() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, width) in [(&b"added"[..], 6usize), (b"added6", 18)] {
        if let Some(raw) = d.get(key).and_then(Value::as_bytes) {
            for chunk in raw.chunks_exact(width) {
                out.push(decode_compact(chunk));
            }
        }
    }
    out
}

fn decode_compact(b: &[u8]) -> SocketAddr {
    let port = u16::from_be_bytes([b[b.len() - 2], b[b.len() - 1]]);
    if b.len() == 6 {
        SocketAddr::from(([b[0], b[1], b[2], b[3]], port))
    } else {
        let ip: [u8; 16] = b[..16].try_into().expect("16 bytes");
        SocketAddr::from((ip, port))
    }
}

pub fn pex_message(peers: &[SocketAddr]) -> Vec<u8> {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for p in peers {
        let target = if p.is_ipv4() { &mut v4 } else { &mut v6 };
        match p.ip() {
            std::net::IpAddr::V4(ip) => target.extend_from_slice(&ip.octets()),
            std::net::IpAddr::V6(ip) => target.extend_from_slice(&ip.octets()),
        }
        target.extend_from_slice(&p.port().to_be_bytes());
    }
    let mut out = Vec::new();
    Encoder::new(&mut out).dict(|d| {
        d.entry(b"added").bytes(&v4);
        d.entry(b"added6").bytes(&v6);
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_roundtrip() {
        let hs = handshake(&[1; 20], &[2; 20], true);
        assert_eq!(hs.len(), 68);
        let parsed = parse_handshake(&hs).unwrap();
        assert!(parsed.extensions);
        assert_eq!(parsed.info_hash, [1; 20]);
        assert_eq!(parsed.peer_id, [2; 20]);
        assert!(
            !parse_handshake(&handshake(&[1; 20], &[2; 20], false))
                .unwrap()
                .extensions
        );
        let mut bad = hs;
        bad[3] = b'X';
        assert!(parse_handshake(&bad).is_err());
    }

    #[test]
    fn extended_handshake_roundtrip_and_disabled_ids() {
        let ours = parse_extended_handshake(&our_extended_handshake(Some(1234))).unwrap();
        assert_eq!(ours.ut_metadata, Some(OUR_UT_METADATA));
        assert_eq!(ours.ut_pex, Some(OUR_UT_PEX));
        assert_eq!(ours.metadata_size, Some(1234));
        assert_eq!(ours.client.as_deref(), Some("Lodestar"));
        let disabled = parse_extended_handshake(b"d1:md11:ut_metadatai0eee").unwrap();
        assert_eq!(disabled.ut_metadata, None);
        assert!(parse_extended_handshake(b"not bencode").is_err());
    }

    #[test]
    fn metadata_messages_roundtrip() {
        assert_eq!(
            parse_metadata_message(&metadata_request(3)).unwrap(),
            MetadataMessage::Request { piece: 3 }
        );
        assert_eq!(
            parse_metadata_message(&metadata_reject(4)).unwrap(),
            MetadataMessage::Reject { piece: 4 }
        );
        let data = metadata_data(1, 20_000, b"payload");
        assert_eq!(
            parse_metadata_message(&data).unwrap(),
            MetadataMessage::Data {
                piece: 1,
                total_size: Some(20_000),
                data: b"payload"
            }
        );
        assert!(parse_metadata_message(b"d8:msg_typei9e5:piecei0ee").is_err());
    }

    #[test]
    fn pex_roundtrip() {
        let peers: Vec<SocketAddr> = vec![
            "1.2.3.4:5".parse().unwrap(),
            "[2001:db8::1]:6".parse().unwrap(),
        ];
        assert_eq!(parse_pex(&pex_message(&peers)), peers);
        assert!(parse_pex(b"garbage").is_empty());
        // Trailing partial entries are ignored.
        assert_eq!(parse_pex(b"d5:added7:\x01\x02\x03\x04\x00\x05Xe").len(), 1);
    }
}
