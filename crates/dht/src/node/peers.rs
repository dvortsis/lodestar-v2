//! A small store of announced peers (BEP 5), so `get_peers` can be
//! answered with `values`. Bounded in hashes and peers per hash; entries
//! expire after 30 minutes, as announces are re-sent periodically. The
//! announce's `seed` flag is kept so BEP 33 scrapes can be answered.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::time::Instant;

use super::transport::Map;

use lodestar_metainfo::InfoHash;

use crate::bep33::Bloom;

pub(crate) const TTL: Duration = Duration::from_secs(30 * 60);
const MAX_HASHES: usize = 10_000;
const MAX_PEERS: usize = 50;

#[derive(Debug, Default)]
pub(crate) struct PeerStore {
    map: Map<InfoHash, Vec<Entry>>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    peer: SocketAddr,
    at: Instant,
    seed: bool,
}

impl PeerStore {
    pub fn announce(&mut self, hash: InfoHash, peer: SocketAddr, seed: bool, now: Instant) {
        if !self.map.contains_key(&hash) && self.map.len() >= MAX_HASHES {
            return;
        }
        let peers = self.map.entry(hash).or_default();
        let entry = Entry {
            peer,
            at: now,
            seed,
        };
        if let Some(e) = peers.iter_mut().find(|e| e.peer == peer) {
            *e = entry;
        } else if peers.len() < MAX_PEERS {
            peers.push(entry);
        } else if let Some(oldest) = peers.iter_mut().min_by_key(|e| e.at) {
            *oldest = entry;
        }
    }

    /// Live peers of one address family; seeds are left out for `noseed`
    /// (BEP 33).
    pub fn peers(
        &self,
        hash: &InfoHash,
        ipv6: bool,
        noseed: bool,
        now: Instant,
    ) -> Vec<SocketAddr> {
        self.live(hash, now)
            .filter(|e| e.peer.is_ipv6() == ipv6 && !(noseed && e.seed))
            .map(|e| e.peer)
            .collect()
    }

    /// BEP 33 `(BFsd, BFpe)` over the live peers of both families, or
    /// `None` when we store no peers for the hash.
    pub fn blooms(&self, hash: &InfoHash, now: Instant) -> Option<(Bloom, Bloom)> {
        let mut out: Option<(Bloom, Bloom)> = None;
        for e in self.live(hash, now) {
            let (seeds, peers) = out.get_or_insert_with(Default::default);
            if e.seed {
                seeds.insert(e.peer.ip());
            } else {
                peers.insert(e.peer.ip());
            }
        }
        out
    }

    fn live<'a>(&'a self, hash: &InfoHash, now: Instant) -> impl Iterator<Item = &'a Entry> + 'a {
        self.map
            .get(hash)
            .into_iter()
            .flatten()
            .filter(move |e| now.duration_since(e.at) < TTL)
    }

    pub fn expire(&mut self, now: Instant) {
        self.map.retain(|_, peers| {
            peers.retain(|e| now.duration_since(e.at) < TTL);
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
        s.announce(h, p4, false, t0);
        s.announce(h, p4, false, t0);
        s.announce(h, p6, false, t0);
        assert_eq!(s.peers(&h, false, false, t0), [p4]);
        assert_eq!(s.peers(&h, true, false, t0), [p6]);
        s.announce(h, p4, false, t0 + TTL / 2);
        s.expire(t0 + TTL);
        assert_eq!(
            s.peers(&h, false, false, t0 + TTL),
            [p4],
            "refreshed entry survives"
        );
        assert!(s.peers(&h, true, false, t0 + TTL).is_empty());
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
                false,
                t0 + Duration::from_secs(u64::from(i)),
            );
        }
        let peers = s.peers(&h, false, false, t0 + Duration::from_secs(200));
        assert_eq!(peers.len(), MAX_PEERS);
        assert!(
            peers.contains(&SocketAddr::from(([10, 0, 0, 1], 200))),
            "newest kept"
        );
    }

    #[test]
    fn seeds_feed_bfsd_and_noseed_hides_them() {
        let mut s = PeerStore::default();
        let t0 = Instant::now();
        let h = InfoHash([3; 20]);
        assert!(s.blooms(&h, t0).is_none(), "nothing stored, no filters");
        let seed: SocketAddr = "10.0.0.1:1".parse().unwrap();
        let leech: SocketAddr = "10.0.0.2:1".parse().unwrap();
        let leech6: SocketAddr = "[2001:db8::2]:1".parse().unwrap();
        s.announce(h, seed, true, t0);
        s.announce(h, leech, false, t0);
        s.announce(h, leech6, false, t0);
        let (sd, pe) = s.blooms(&h, t0).unwrap();
        assert_eq!((sd.count(), pe.count()), (1, 2), "both families count");
        assert_eq!(s.peers(&h, false, true, t0), [leech]);
        assert_eq!(s.peers(&h, false, false, t0).len(), 2);
        // A seed that announces again without the flag is a leecher now.
        s.announce(h, seed, false, t0);
        let (sd, pe) = s.blooms(&h, t0).unwrap();
        assert_eq!((sd.count(), pe.count()), (0, 3));
        assert!(s.blooms(&h, t0 + TTL).is_none(), "expired");
    }
}
