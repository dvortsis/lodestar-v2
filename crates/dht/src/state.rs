//! Warm-restart state: `data/dht/state.bin` (architecture §4, §5).
//!
//! Holds our node ids, the last external IP seen in BEP 42 `ip` fields and
//! the good nodes from the routing tables, so a restart rejoins the DHT
//! through known-good nodes instead of only the public routers.
//!
//! The format is a canonical bencode dictionary, versioned:
//!
//! ```text
//! { "ids": <20-byte ids, concatenated>, "ip": <4 or 16 bytes>,
//!   "nodes": <compact IPv4 nodes>, "nodes6": <compact IPv6 nodes>, "v": 1 }
//! ```
//!
//! Writes go to a temporary file that is synced and renamed over the old
//! one, so a crash never leaves a torn file. A missing or unreadable file
//! just means a cold start.

use std::fmt;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

use lodestar_bencode::{self as bencode, DecodeOptions, Encoder, Value};

use crate::compact::{self, CompactNodes, NodeId, NodeInfo};

pub const VERSION: i64 = 1;
/// More than enough to rejoin; bounds file size and load work.
pub const MAX_NODES: usize = 4096;
pub const MAX_IDS: usize = 64;
const MAX_FILE: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    pub ids: Vec<NodeId>,
    pub external_ip: Option<IpAddr>,
    pub nodes: Vec<NodeInfo>,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum StateError {
    Io(io::Error),
    Corrupt(&'static str),
    UnsupportedVersion(i64),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateError::Io(e) => write!(f, "dht state: {e}"),
            StateError::Corrupt(what) => write!(f, "dht state: corrupt ({what})"),
            StateError::UnsupportedVersion(v) => write!(f, "dht state: unsupported version {v}"),
        }
    }
}

impl std::error::Error for StateError {}

impl From<io::Error> for StateError {
    fn from(e: io::Error) -> Self {
        StateError::Io(e)
    }
}

impl State {
    /// Serializes, keeping at most [`MAX_IDS`] ids and [`MAX_NODES`] nodes.
    pub fn encode(&self) -> Vec<u8> {
        let nodes = &self.nodes[..self.nodes.len().min(MAX_NODES)];
        let ids: Vec<u8> = self.ids.iter().take(MAX_IDS).flat_map(|id| id.0).collect();
        let mut out = Vec::new();
        Encoder::new(&mut out).dict(|d| {
            d.entry(b"ids").bytes(&ids);
            match self.external_ip {
                Some(IpAddr::V4(ip)) => d.entry(b"ip").bytes(&ip.octets()),
                Some(IpAddr::V6(ip)) => d.entry(b"ip").bytes(&ip.octets()),
                None => {}
            }
            d.entry(b"nodes")
                .bytes(&compact::encode_nodes(nodes, false));
            d.entry(b"nodes6")
                .bytes(&compact::encode_nodes(nodes, true));
            d.entry(b"v").int(VERSION);
        });
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<State, StateError> {
        if bytes.len() > MAX_FILE {
            return Err(StateError::Corrupt("too large"));
        }
        let limits = bencode::Limits {
            max_depth: 2,
            max_items: 16,
        };
        let value = bencode::decode_with(bytes, &DecodeOptions::strict().with_limits(limits))
            .map_err(|_| StateError::Corrupt("not canonical bencode"))?;
        let d = value
            .as_dict()
            .ok_or(StateError::Corrupt("not a dictionary"))?;
        match d.get(b"v").and_then(Value::as_int) {
            Some(VERSION) => {}
            Some(v) => return Err(StateError::UnsupportedVersion(v)),
            None => return Err(StateError::Corrupt("missing version")),
        }
        let field = |key: &[u8]| d.get(key).and_then(Value::as_bytes).unwrap_or_default();

        let ids_raw = field(b"ids");
        if ids_raw.len() % 20 != 0 || ids_raw.len() / 20 > MAX_IDS {
            return Err(StateError::Corrupt("ids"));
        }
        let ids = ids_raw
            .chunks_exact(20)
            .map(|c| NodeId(c.try_into().expect("20 bytes")))
            .collect();

        let external_ip = match field(b"ip") {
            [] => None,
            ip if ip.len() == 4 => Some(IpAddr::V4(Ipv4Addr::from(
                <[u8; 4]>::try_from(ip).expect("4 bytes"),
            ))),
            ip if ip.len() == 16 => Some(IpAddr::V6(Ipv6Addr::from(
                <[u8; 16]>::try_from(ip).expect("16 bytes"),
            ))),
            _ => return Err(StateError::Corrupt("ip")),
        };

        let v4 = CompactNodes::v4(field(b"nodes")).ok_or(StateError::Corrupt("nodes"))?;
        let v6 = CompactNodes::v6(field(b"nodes6")).ok_or(StateError::Corrupt("nodes6"))?;
        if v4.len() + v6.len() > MAX_NODES {
            return Err(StateError::Corrupt("too many nodes"));
        }
        let nodes = v4.iter().chain(v6.iter()).collect();
        Ok(State {
            ids,
            external_ip,
            nodes,
        })
    }

    /// Loads state; `Ok(None)` when there is no file yet.
    pub fn load(path: &Path) -> Result<Option<State>, StateError> {
        match std::fs::read(path) {
            Ok(bytes) => State::decode(&bytes).map(Some),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes atomically: temp file, fsync, rename.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&self.encode())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> State {
        State {
            ids: vec![NodeId([1; 20]), NodeId([2; 20])],
            external_ip: Some("203.0.113.7".parse().unwrap()),
            nodes: vec![
                NodeInfo {
                    id: NodeId([3; 20]),
                    addr: "198.51.100.1:6881".parse().unwrap(),
                },
                NodeInfo {
                    id: NodeId([4; 20]),
                    addr: "[2001:db8::4]:6881".parse().unwrap(),
                },
            ],
        }
    }

    #[test]
    fn roundtrip_through_a_file() {
        let dir = std::env::temp_dir().join(format!("lodestar-dht-state-{}", std::process::id()));
        let path = dir.join("dht/state.bin");
        assert!(
            State::load(&path).unwrap().is_none(),
            "missing file means cold start"
        );
        let s = sample();
        s.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap().unwrap(), s);
        // Overwrite in place.
        let empty = State::default();
        empty.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap().unwrap(), empty);
        assert!(!path.with_extension("tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn caps_are_applied_on_encode() {
        let s = State {
            ids: (0..100).map(|i| NodeId([i as u8; 20])).collect(),
            external_ip: None,
            nodes: (0..5000u32)
                .map(|i| NodeInfo {
                    id: NodeId([7; 20]),
                    addr: std::net::SocketAddr::from((
                        [10, (i >> 16) as u8, (i >> 8) as u8, i as u8],
                        1,
                    )),
                })
                .collect(),
        };
        let back = State::decode(&s.encode()).unwrap();
        assert_eq!(back.ids.len(), MAX_IDS);
        assert_eq!(back.nodes.len(), MAX_NODES);
    }

    #[test]
    fn corrupt_files_are_rejected() {
        let good = sample().encode();
        assert!(State::decode(&good[..good.len() - 1]).is_err());
        assert!(State::decode(b"de").is_err());
        assert!(matches!(
            State::decode(b"d1:vi2ee"),
            Err(StateError::UnsupportedVersion(2))
        ));
        assert!(matches!(
            State::decode(b"d3:ids3:abc1:vi1ee"),
            Err(StateError::Corrupt("ids"))
        ));
        assert!(matches!(
            State::decode(b"d2:ip3:abc1:vi1ee"),
            Err(StateError::Corrupt("ip"))
        ));
        assert!(matches!(
            State::decode(b"d5:nodes3:abc1:vi1ee"),
            Err(StateError::Corrupt("nodes"))
        ));
    }
}
