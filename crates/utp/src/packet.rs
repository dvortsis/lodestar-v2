//! The BEP 29 packet format: a 20-byte header, a chain of extensions (only
//! selective ACK is understood; others are skipped), then the payload.
//!
//! Every packet comes off the network, so parsing checks every length
//! before using it and never allocates.

use std::fmt;

pub const HEADER_LEN: usize = 20;
pub const VERSION: u8 = 1;
/// Extension 1: selective ACK.
const EXT_SACK: u8 = 1;
/// More extensions than this in one packet is malformed (BEP 29 defines
/// two; real packets carry at most one).
const MAX_EXTENSIONS: usize = 8;
/// The largest selective-ACK bitmask we send or accept: 256 packets.
pub const MAX_SACK_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketType {
    Data = 0,
    Fin = 1,
    State = 2,
    Reset = 3,
    Syn = 4,
}

impl PacketType {
    fn from_nibble(n: u8) -> Option<PacketType> {
        Some(match n {
            0 => PacketType::Data,
            1 => PacketType::Fin,
            2 => PacketType::State,
            3 => PacketType::Reset,
            4 => PacketType::Syn,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub ty: PacketType,
    pub conn_id: u16,
    /// The sender's clock, in microseconds (wrapping).
    pub timestamp_us: u32,
    /// The sender's clock minus our timestamp on the last packet it got.
    pub timestamp_diff_us: u32,
    /// Bytes the sender can still receive.
    pub wnd_size: u32,
    pub seq_nr: u16,
    pub ack_nr: u16,
}

/// A parsed packet borrowing from the datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet<'a> {
    pub header: Header,
    /// The selective-ACK bitmask: bit `i` (LSB first in each byte) of byte
    /// `j` stands for packet `ack_nr + 2 + 8j + i`.
    pub sack: Option<&'a [u8]>,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    TooShort,
    Version,
    Type,
    /// An extension runs past the end, repeats, or has a bad length.
    Extension,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ParseError::TooShort => "packet shorter than its header",
            ParseError::Version => "not uTP version 1",
            ParseError::Type => "unknown packet type",
            ParseError::Extension => "malformed extension",
        })
    }
}

impl std::error::Error for ParseError {}

/// Whether the first byte reads as a uTP v1 header (type 0–4, version 1).
/// KRPC messages start with `d` (0x64): type 6, version 4, so the two never
/// collide.
pub fn looks_like_utp(datagram: &[u8]) -> bool {
    datagram.len() >= HEADER_LEN
        && datagram[0] & 0x0f == VERSION
        && PacketType::from_nibble(datagram[0] >> 4).is_some()
}

pub fn parse(buf: &[u8]) -> Result<Packet<'_>, ParseError> {
    if buf.len() < HEADER_LEN {
        return Err(ParseError::TooShort);
    }
    if buf[0] & 0x0f != VERSION {
        return Err(ParseError::Version);
    }
    let ty = PacketType::from_nibble(buf[0] >> 4).ok_or(ParseError::Type)?;
    let u16_at = |i: usize| u16::from_be_bytes([buf[i], buf[i + 1]]);
    let u32_at = |i: usize| u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
    let header = Header {
        ty,
        conn_id: u16_at(2),
        timestamp_us: u32_at(4),
        timestamp_diff_us: u32_at(8),
        wnd_size: u32_at(12),
        seq_nr: u16_at(16),
        ack_nr: u16_at(18),
    };
    let mut ext = buf[1];
    let mut at = HEADER_LEN;
    let mut sack = None;
    let mut count = 0;
    while ext != 0 {
        count += 1;
        if count > MAX_EXTENSIONS || buf.len() < at + 2 {
            return Err(ParseError::Extension);
        }
        let (next, len) = (buf[at], usize::from(buf[at + 1]));
        let body = buf.get(at + 2..at + 2 + len).ok_or(ParseError::Extension)?;
        if ext == EXT_SACK {
            // At least 32 bits, in whole 32-bit words (BEP 29).
            if sack.is_some() || len < 4 || len % 4 != 0 || len > MAX_SACK_LEN {
                return Err(ParseError::Extension);
            }
            sack = Some(body);
        }
        at += 2 + len;
        ext = next;
    }
    Ok(Packet {
        header,
        sack,
        payload: &buf[at..],
    })
}

/// Appends the encoded packet to `out`. A `sack` must be 4–32 bytes in
/// whole words (debug-asserted; [`parse`] rejects anything else).
pub fn encode(header: &Header, sack: Option<&[u8]>, payload: &[u8], out: &mut Vec<u8>) {
    debug_assert!(sack.is_none_or(|s| s.len() >= 4 && s.len() % 4 == 0 && s.len() <= MAX_SACK_LEN));
    out.reserve(HEADER_LEN + sack.map_or(0, |s| s.len() + 2) + payload.len());
    out.push((header.ty as u8) << 4 | VERSION);
    out.push(if sack.is_some() { EXT_SACK } else { 0 });
    out.extend_from_slice(&header.conn_id.to_be_bytes());
    out.extend_from_slice(&header.timestamp_us.to_be_bytes());
    out.extend_from_slice(&header.timestamp_diff_us.to_be_bytes());
    out.extend_from_slice(&header.wnd_size.to_be_bytes());
    out.extend_from_slice(&header.seq_nr.to_be_bytes());
    out.extend_from_slice(&header.ack_nr.to_be_bytes());
    if let Some(s) = sack {
        out.push(0);
        out.push(s.len() as u8);
        out.extend_from_slice(s);
    }
    out.extend_from_slice(payload);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn header(ty: PacketType) -> Header {
        Header {
            ty,
            conn_id: 0xbeef,
            timestamp_us: 1,
            timestamp_diff_us: 2,
            wnd_size: 3,
            seq_nr: 4,
            ack_nr: 5,
        }
    }

    #[test]
    fn known_bytes() {
        let mut out = Vec::new();
        encode(&header(PacketType::Syn), None, b"", &mut out);
        assert_eq!(
            out,
            [
                0x41, 0, 0xbe, 0xef, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 4, 0, 5
            ]
        );
        assert!(looks_like_utp(&out));
    }

    #[test]
    fn demux_never_mistakes_krpc() {
        let krpc = b"d1:ad2:id20:abcdefghij0123456789e1:q4:ping1:t2:aa1:y1:qe";
        assert!(!looks_like_utp(krpc));
        assert!(!looks_like_utp(&[0x41; 19]), "too short");
        assert!(!looks_like_utp(&[0x51; 20]), "type 5");
        assert!(!looks_like_utp(&[0x42; 20]), "version 2");
        for ty in 0..=4u8 {
            assert!(looks_like_utp(&[ty << 4 | 1; 20]));
        }
    }

    #[test]
    fn rejects_malformed() {
        let mut ok = Vec::new();
        encode(
            &header(PacketType::State),
            Some(&[1, 0, 0, 0]),
            b"",
            &mut ok,
        );
        assert!(parse(&ok).is_ok());
        assert_eq!(parse(&ok[..19]), Err(ParseError::TooShort));
        // The extension runs past the end.
        assert_eq!(parse(&ok[..ok.len() - 1]), Err(ParseError::Extension));
        let mut bad_len = ok.clone();
        bad_len[21] = 3;
        assert_eq!(parse(&bad_len), Err(ParseError::Extension));
        let mut v2 = ok.clone();
        v2[0] = 0x22;
        assert_eq!(parse(&v2), Err(ParseError::Version));
        let mut ty = ok.clone();
        ty[0] = 0x71;
        assert_eq!(parse(&ty), Err(ParseError::Type));
        // Two selective ACKs.
        let mut twice = ok[..20].to_vec();
        twice[1] = 1;
        twice.extend_from_slice(&[1, 4, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0]);
        assert_eq!(parse(&twice), Err(ParseError::Extension));
        // An endless chain of unknown extensions.
        let mut chain = ok[..20].to_vec();
        chain[1] = 9;
        for _ in 0..MAX_EXTENSIONS + 1 {
            chain.extend_from_slice(&[9, 0]);
        }
        assert_eq!(parse(&chain), Err(ParseError::Extension));
    }

    #[test]
    fn skips_unknown_extensions() {
        let mut buf = Vec::new();
        encode(&header(PacketType::Data), None, b"", &mut buf);
        buf[1] = 2; // the legacy "extension bits", 8 bytes
        buf.extend_from_slice(&[1, 8, 0, 0, 0, 0, 0, 0, 0, 0]);
        buf.extend_from_slice(&[0, 4, 0xff, 0, 0, 0]);
        buf.extend_from_slice(b"payload");
        let p = parse(&buf).unwrap();
        assert_eq!(p.sack, Some(&[0xff, 0, 0, 0][..]));
        assert_eq!(p.payload, b"payload");
    }

    fn any_type() -> impl Strategy<Value = PacketType> {
        prop_oneof![
            Just(PacketType::Data),
            Just(PacketType::Fin),
            Just(PacketType::State),
            Just(PacketType::Reset),
            Just(PacketType::Syn),
        ]
    }

    proptest! {
        #[test]
        fn round_trip(
            ty in any_type(),
            fields in any::<(u16, u32, u32, u32, u16, u16)>(),
            sack in proptest::option::of((1usize..=8).prop_flat_map(|w| proptest::collection::vec(any::<u8>(), w * 4))),
            payload in proptest::collection::vec(any::<u8>(), 0..1500),
        ) {
            let h = Header {
                ty,
                conn_id: fields.0,
                timestamp_us: fields.1,
                timestamp_diff_us: fields.2,
                wnd_size: fields.3,
                seq_nr: fields.4,
                ack_nr: fields.5,
            };
            let mut out = Vec::new();
            encode(&h, sack.as_deref(), &payload, &mut out);
            let p = parse(&out).unwrap();
            prop_assert_eq!(p.header, h);
            prop_assert_eq!(p.sack, sack.as_deref());
            prop_assert_eq!(p.payload, &payload[..]);
            prop_assert!(looks_like_utp(&out));
        }

        #[test]
        fn parse_never_panics_and_reencodes(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            if let Ok(p) = parse(&bytes) {
                let mut out = Vec::new();
                encode(&p.header, p.sack, p.payload, &mut out);
                prop_assert_eq!(parse(&out).unwrap(), p);
            }
        }
    }
}
