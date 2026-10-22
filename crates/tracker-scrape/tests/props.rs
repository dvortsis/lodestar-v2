//! The reply parsers on arbitrary input: no panics, and replies built
//! to the protocol parse back to what was encoded.

use lodestar_metainfo::InfoHash;
use lodestar_tracker_scrape::udp::{self, Packet};
use lodestar_tracker_scrape::{Counts, http};
use proptest::prelude::*;

fn counts() -> impl Strategy<Value = Counts> {
    (any::<u32>(), any::<u32>(), any::<u32>()).prop_map(|(seeders, completed, leechers)| Counts {
        seeders,
        completed,
        leechers,
    })
}

proptest! {
    #[test]
    fn udp_parser_never_panics(p in proptest::collection::vec(any::<u8>(), 0..1200)) {
        let _ = udp::parse_response(&p);
    }

    #[test]
    fn http_parser_never_panics(
        body in proptest::collection::vec(any::<u8>(), 0..600),
        n in 0usize..4,
    ) {
        let hashes: Vec<InfoHash> = (0..n as u8).map(|i| InfoHash([i; 20])).collect();
        if let Ok(r) = http::parse_scrape(&body, &hashes) {
            prop_assert_eq!(r.counts.len(), n);
        }
    }

    #[test]
    fn udp_scrape_replies_round_trip(tid in any::<u32>(), cs in proptest::collection::vec(counts(), 0..=udp::MAX_HASHES)) {
        let mut p = 2u32.to_be_bytes().to_vec();
        p.extend_from_slice(&tid.to_be_bytes());
        for c in &cs {
            for v in [c.seeders, c.completed, c.leechers] {
                p.extend_from_slice(&v.to_be_bytes());
            }
        }
        prop_assert_eq!(udp::parse_response(&p).unwrap(), Packet::Scrape { transaction: tid, counts: cs });
    }

    #[test]
    fn http_replies_round_trip(cs in proptest::collection::vec(counts(), 1..20)) {
        let hashes: Vec<InfoHash> = (0..cs.len() as u8).map(|i| InfoHash([i; 20])).collect();
        let mut body = Vec::new();
        lodestar_bencode::Encoder::new(&mut body).dict(|d| {
            d.entry(b"files").dict(|f| {
                for (h, c) in hashes.iter().zip(&cs) {
                    f.entry(&h.0).dict(|e| {
                        e.entry(b"complete").int(i64::from(c.seeders));
                        e.entry(b"downloaded").int(i64::from(c.completed));
                        e.entry(b"incomplete").int(i64::from(c.leechers));
                    });
                }
            });
        });
        let r = http::parse_scrape(&body, &hashes).unwrap();
        prop_assert_eq!(r.counts, cs.into_iter().map(Some).collect::<Vec<_>>());
    }
}
