//! Property tests for SRC-1: the claim encoding (an untrusted-input
//! parser; `fuzz/fuzz_targets/claims.rs` runs the same checks unguided)
//! and promotion keeping ids.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_store::claims::{
    self, ClaimedFile, Claims, Confidence, ExternalId, MAX_PLAIN, Provenance,
};
use lodestar_store::unverified::{Network, NewUnverified, Promoted, Put};
use lodestar_store::{PreparedTorrent, Store, StoreConfig, WriteBatch};
use proptest::prelude::*;

fn text(max: usize) -> impl Strategy<Value = String> {
    proptest::string::string_regex(&format!("(.|\\n){{1,{}}}", max.min(64)))
        .unwrap()
        .prop_filter("within bytes", move |s| !s.is_empty() && s.len() <= max)
}

fn claims() -> impl Strategy<Value = Claims> {
    let files = proptest::collection::vec(
        (
            proptest::string::string_regex(".{0,40}").unwrap(),
            proptest::option::of(any::<u64>()),
        )
            .prop_map(|(path, size)| ClaimedFile { path, size }),
        0..20,
    );
    let ids = proptest::collection::vec(
        ("[a-z0-9]{1,12}", text(32)).prop_map(|(scheme, value)| ExternalId { scheme, value }),
        0..5,
    );
    let prov = (
        proptest::option::of(text(200)),
        proptest::option::of(any::<i64>()),
        0u8..3,
    )
        .prop_map(|(external_ref, published_at, c)| Provenance {
            external_ref,
            published_at,
            confidence: [Confidence::Low, Confidence::Medium, Confidence::High][c as usize],
        });
    (
        (
            proptest::option::of(text(1024)),
            proptest::option::of(any::<u64>()),
            files,
            proptest::collection::vec(text(100), 0..5),
        ),
        (
            ids,
            proptest::option::of(text(500)),
            proptest::option::of(text(256)),
            prov,
        ),
    )
        .prop_map(
            |((name, size, files, trackers), (ids, description, publisher, provenance))| Claims {
                name,
                size,
                files,
                trackers,
                ids,
                description,
                publisher,
                provenance,
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// What's encoded decodes to the same claims, at any level.
    #[test]
    fn claims_round_trip(c in claims(), level in 1i32..6) {
        let blob = claims::encode(&c, level).unwrap();
        prop_assert!(blob.len() <= claims::MAX_STORED);
        prop_assert_eq!(claims::decode(&blob).unwrap(), c);
    }

    /// Any bytes: no panic, and an accepted body is canonical and within
    /// every limit.
    #[test]
    fn any_bytes_decode_safely(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = claims::decode(&bytes);
        if let Ok(c) = claims::decode_plain(&bytes) {
            prop_assert!(c.check().is_ok());
            prop_assert_eq!(claims::encode_plain(&c).unwrap(), bytes);
        }
    }

    /// A valid body with one byte changed, cut short or extended: an error
    /// or claims within the limits, never a panic.
    #[test]
    fn mutated_bodies_decode_safely(
        c in claims(),
        at in any::<prop::sample::Index>(),
        byte in any::<u8>(),
        cut in any::<prop::sample::Index>(),
    ) {
        let plain = claims::encode_plain(&c).unwrap();
        prop_assert!(plain.len() <= MAX_PLAIN);
        let mut m = plain.clone();
        let i = at.index(m.len());
        m[i] = byte;
        if let Ok(d) = claims::decode_plain(&m) {
            prop_assert!(d.check().is_ok());
        }
        let short = &plain[..cut.index(plain.len())];
        prop_assert!(claims::decode_plain(short).is_err());
        let mut long = plain.clone();
        long.push(byte);
        prop_assert!(claims::decode_plain(&long).is_err());
    }
}

fn prepared(store: &Store, n: u32) -> lodestar_store::Prepared {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(5)),
        (b"name".to_vec(), b(format!("t{n}").as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(16_384)),
        (b"pieces".to_vec(), b(&[0; 20])),
    ];
    let info = Info::parse(&to_vec(&OwnedValue::Dict(e.into_iter().collect()))).unwrap();
    store.preparer().prepare(&info, None).unwrap()
}

#[derive(Debug, Clone)]
enum Op {
    /// A source finds torrent `n` (unverified).
    Find(u8),
    /// The crawler stores torrent `n` directly.
    Crawl(u8),
    /// Torrent `n`'s metadata arrives for its unverified item.
    Promote(u8),
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Whatever the interleaving, an item promoted while no torrent held
    /// its hash keeps its id; ids are never shared or reused; and the
    /// sequence only moves forward.
    #[test]
    fn promotion_keeps_the_id(ops in proptest::collection::vec(
        prop_oneof![
            (0u8..12).prop_map(Op::Find),
            (0u8..12).prop_map(Op::Crawl),
            (0u8..12).prop_map(Op::Promote),
        ],
        1..40,
    )) {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 0).unwrap();
        let nyaa = store.register_source("nyaa", "1", 0).unwrap();
        let ts: Vec<_> = (0..12).map(|n| prepared(&store, n)).collect();
        let name = Claims { name: Some("x".into()), ..Claims::default() };
        let mut issued = std::collections::BTreeSet::new();
        let next = |store: &Store| -> i64 {
            store.connection().query_row("SELECT next FROM id_seq", [], |r| r.get(0)).unwrap()
        };
        let mut last_next = next(&store);
        for op in ops {
            match op {
                Op::Find(n) => {
                    let put = store.put_unverified(&[NewUnverified {
                        network: Network::Bt,
                        key: &ts[n as usize].info_hash.0,
                        source: nyaa,
                        private_source: false,
                        claims: Some(&name),
                        at: 0,
                    }]).unwrap()[0];
                    if let Put::New(id) = put {
                        prop_assert!(issued.insert(id), "id {} issued twice", id);
                    }
                }
                Op::Crawl(n) => {
                    let w = store.write(&WriteBatch {
                        prepared: &[PreparedTorrent {
                            torrent: &ts[n as usize],
                            discovered_at: 0,
                            metadata_at: 0,
                            extra_flags: 0,
                            source: dht,
                            verified: true,
                        }],
                        ..WriteBatch::default()
                    }).unwrap();
                    if let lodestar_store::Inserted::New(id) = w.prepared[0] {
                        prop_assert!(issued.insert(id), "id {} issued twice", id);
                    }
                }
                Op::Promote(n) => {
                    let p = &ts[n as usize];
                    let Some(item) = store.unverified(Network::Bt, &p.info_hash.0).unwrap() else {
                        continue;
                    };
                    let stored = store.find(&p.info_hash).unwrap();
                    let out = store.promote(item.id, &PreparedTorrent {
                        torrent: p,
                        discovered_at: 0,
                        metadata_at: 0,
                        extra_flags: 0,
                        source: dht,
                        verified: true,
                    }).unwrap();
                    match stored {
                        None => prop_assert_eq!(out, Promoted::Kept(item.id)),
                        Some(t) => prop_assert_eq!(out, Promoted::Merged(t)),
                    }
                    prop_assert_eq!(store.find(&p.info_hash).unwrap(), Some(match out {
                        Promoted::Kept(id) | Promoted::Merged(id) => id,
                        _ => unreachable!(),
                    }));
                }
            }
            let now = next(&store);
            prop_assert!(now >= last_next);
            prop_assert!(issued.iter().all(|id| *id < now));
            last_next = now;
        }
    }
}
