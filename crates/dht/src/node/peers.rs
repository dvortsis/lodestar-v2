//! A small store of announced peers (BEP 5), so `get_peers` can be
//! answered with `values`. Bounded in hashes and peers per hash; entries
//! expire after 30 minutes, as announces are re-sent periodically.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use lodestar_metainfo::InfoHash;

pub(crate) const TTL: Duration = Duration::from_secs(30 * 60);
const MAX_HASHES: usize = 10_000;
const MAX_PEERS: usize = 50;

#[derive(Debug, Default)]
pub(crate) struct PeerStore {
    map: HashMap<InfoHash, Vec<(SocketAddr, Instant)>>,
}

impl PeerStore {
    pub fn announce(&mut self, hash: InfoHash, peer: SocketAddr, now: Instant) {
        if !self.map.contains_key(&hash) && self.map.len() >= MAX_HASHES {
            return;
        }
        let peers = self.map.entry(hash).or_default();
        if let Some(entry) = peers.iter_mut().find(|(p, _)| *p == peer) {
            entry.1 = now;
        } else if peers.len() < MAX_PEERS {
            peers.push((peer, now));
        } else if let Some(oldest) = peers.iter_mut().min_by_key(|(_, t)| *t) {
            *oldest = (peer, now);
        }
    }

    /// Live peers of one address family.
    pub fn peers(&self, hash: &InfoHash, ipv6: bool, now: Instant) -> Vec<SocketAddr> {
        self.map
            .get(hash)
            .map(|peers| {
                peers
                    .iter()
                    .filter(|(p, t)| p.is_ipv6() == ipv6 && now.duration_since(*t) < TTL)
                    .map(|(p, _)| *p)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn expire(&mut self, now: Instant) {
        self.map.retain(|_, peers| {
            peers.retain(|(_, t)| now.duration_since(*t) < TTL);
            !peers.is_empty()
        });
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_refreshes_and_expires() {
        let mut s = PeerStore::default();
        let t0 = Instant::now();
        let h = InfoHash([1; 20]);
        let p4: SocketAddr = "1.2.3.4:5".parse().unwrap();
        let p6: SocketAddr = "[::1]:5".parse().unwrap();
        s.announce(h, p4, t0);
        s.announce(h, p4, t0);
        s.announce(h, p6, t0);
        assert_eq!(s.peers(&h, false, t0), [p4]);
        assert_eq!(s.peers(&h, true, t0), [p6]);
        s.announce(h, p4, t0 + TTL / 2);
        s.expire(t0 + TTL);
        assert_eq!(
            s.peers(&h, false, t0 + TTL),
            [p4],
            "refreshed entry survives"
        );
        assert!(s.peers(&h, true, t0 + TTL).is_empty());
        s.expire(t0 + TTL * 2);
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn bounded_per_hash() {
        let mut s = PeerStore::default();
        let t0 = Instant::now();
        let h = InfoHash([2; 20]);
        for i in 0..200u16 {
            s.announce(
                h,
                SocketAddr::from(([10, 0, 0, 1], i + 1)),
                t0 + Duration::from_secs(u64::from(i)),
            );
        }
        let peers = s.peers(&h, false, t0 + Duration::from_secs(200));
        assert_eq!(peers.len(), MAX_PEERS);
        assert!(
            peers.contains(&SocketAddr::from(([10, 0, 0, 1], 200))),
            "newest kept"
        );
    }
}
