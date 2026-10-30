//! Compact binary encodings used inside KRPC messages: node info (BEP 5,
//! BEP 32) and peer/endpoint addresses.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

/// A 160-bit DHT node id.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct NodeId(pub [u8; 20]);

impl NodeId {
    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// XOR distance, the Kademlia metric.
    pub fn distance(&self, other: &NodeId) -> [u8; 20] {
        let mut out = [0u8; 20];
        for (o, (a, b)) in out.iter_mut().zip(self.0.iter().zip(other.0.iter())) {
            *o = a ^ b;
        }
        out
    }

    pub(crate) fn from_slice(b: &[u8]) -> Option<NodeId> {
        b.try_into().ok().map(NodeId)
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NodeId(")?;
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        f.write_str(")")
    }
}

/// A node's id and UDP endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeInfo {
    pub id: NodeId,
    pub addr: SocketAddr,
}

pub const NODE_V4_LEN: usize = 26;
pub const NODE_V6_LEN: usize = 38;

/// Decodes a compact endpoint: 6 bytes (IPv4 + port) or 18 bytes (IPv6 + port).
pub fn decode_addr(b: &[u8]) -> Option<SocketAddr> {
    match b.len() {
        6 => {
            let ip = Ipv4Addr::new(b[0], b[1], b[2], b[3]);
            Some(SocketAddr::V4(SocketAddrV4::new(
                ip,
                u16::from_be_bytes([b[4], b[5]]),
            )))
        }
        18 => {
            let ip: [u8; 16] = b[..16].try_into().ok()?;
            let port = u16::from_be_bytes([b[16], b[17]]);
            Some(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(ip),
                port,
                0,
                0,
            )))
        }
        _ => None,
    }
}

pub fn encode_addr(addr: &SocketAddr, out: &mut Vec<u8>) {
    match addr.ip() {
        IpAddr::V4(ip) => out.extend_from_slice(&ip.octets()),
        IpAddr::V6(ip) => out.extend_from_slice(&ip.octets()),
    }
    out.extend_from_slice(&addr.port().to_be_bytes());
}

/// Concatenated compact node infos of one address family, validated to a
/// whole number of entries.
#[derive(Clone, Copy, Default)]
pub struct CompactNodes<'a> {
    raw: &'a [u8],
    entry: usize,
}

impl<'a> CompactNodes<'a> {
    pub fn v4(raw: &'a [u8]) -> Option<CompactNodes<'a>> {
        raw.len()
            .is_multiple_of(NODE_V4_LEN)
            .then_some(CompactNodes {
                raw,
                entry: NODE_V4_LEN,
            })
    }

    pub fn v6(raw: &'a [u8]) -> Option<CompactNodes<'a>> {
        raw.len()
            .is_multiple_of(NODE_V6_LEN)
            .then_some(CompactNodes {
                raw,
                entry: NODE_V6_LEN,
            })
    }

    pub fn raw(&self) -> &'a [u8] {
        self.raw
    }

    pub fn len(&self) -> usize {
        self.raw.len().checked_div(self.entry).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = NodeInfo> + 'a {
        let entry = self.entry.max(1);
        self.raw.chunks_exact(entry).map(|c| NodeInfo {
            id: NodeId(c[..20].try_into().expect("20-byte id")),
            addr: decode_addr(&c[20..]).expect("validated entry length"),
        })
    }
}

/// Equal when they hold the same entries; any two empty lists are equal.
impl PartialEq for CompactNodes<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw && (self.raw.is_empty() || self.entry == other.entry)
    }
}

impl Eq for CompactNodes<'_> {}

impl fmt::Debug for CompactNodes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// Encodes nodes into the compact form for `nodes` (IPv4 entries) or
/// `nodes6` (IPv6 entries); entries of the other family are skipped.
pub fn encode_nodes(nodes: &[NodeInfo], ipv6: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(nodes.len() * if ipv6 { NODE_V6_LEN } else { NODE_V4_LEN });
    for n in nodes.iter().filter(|n| n.addr.is_ipv6() == ipv6) {
        out.extend_from_slice(&n.id.0);
        encode_addr(&n.addr, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_roundtrip() {
        for addr in ["1.2.3.4:6881", "[2001:db8::1]:51413", "0.0.0.0:0"] {
            let addr: SocketAddr = addr.parse().unwrap();
            let mut out = Vec::new();
            encode_addr(&addr, &mut out);
            assert_eq!(decode_addr(&out), Some(addr));
        }
        assert_eq!(decode_addr(&[0; 5]), None);
        assert_eq!(decode_addr(&[0; 17]), None);
    }

    #[test]
    fn nodes_roundtrip_by_family() {
        let nodes = [
            NodeInfo {
                id: NodeId([1; 20]),
                addr: "10.0.0.1:1".parse().unwrap(),
            },
            NodeInfo {
                id: NodeId([2; 20]),
                addr: "[::1]:2".parse().unwrap(),
            },
        ];
        let v4 = encode_nodes(&nodes, false);
        let v6 = encode_nodes(&nodes, true);
        assert_eq!(v4.len(), NODE_V4_LEN);
        assert_eq!(v6.len(), NODE_V6_LEN);
        assert_eq!(
            CompactNodes::v4(&v4).unwrap().iter().collect::<Vec<_>>(),
            [nodes[0]]
        );
        assert_eq!(
            CompactNodes::v6(&v6).unwrap().iter().collect::<Vec<_>>(),
            [nodes[1]]
        );
        assert!(CompactNodes::v4(&v4[..25]).is_none());
    }

    #[test]
    fn distance_is_xor() {
        let a = NodeId([0xff; 20]);
        let b = NodeId([0x0f; 20]);
        assert_eq!(a.distance(&b), [0xf0; 20]);
        assert_eq!(a.distance(&a), [0; 20]);
    }
}
