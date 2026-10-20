//! Property tests for the KRPC codec.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use lodestar_dht::compact::{CompactNodes, encode_nodes};
use lodestar_dht::krpc::{Body, ErrorBody, Message, Method, Query, Response, Samples, Want, parse};
use lodestar_dht::{NodeId, NodeInfo};
use lodestar_metainfo::InfoHash;
use proptest::prelude::*;

/// Tests need `Message<'static>`; leaking a few bytes per case is fine.
fn leak(v: Vec<u8>) -> &'static [u8] {
    Box::leak(v.into_boxed_slice())
}

fn addr() -> impl Strategy<Value = SocketAddr> {
    prop_oneof![
        (any::<[u8; 4]>(), any::<u16>())
            .prop_map(|(ip, p)| SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), p)),
        (any::<[u8; 16]>(), any::<u16>())
            .prop_map(|(ip, p)| SocketAddr::new(IpAddr::V6(Ipv6Addr::from(ip)), p)),
    ]
}

fn node_id() -> impl Strategy<Value = NodeId> {
    any::<[u8; 20]>().prop_map(NodeId)
}

fn want() -> impl Strategy<Value = Want> {
    (any::<bool>(), any::<bool>()).prop_map(|(v4, v6)| Want { v4, v6 })
}

fn bytes(max: usize) -> impl Strategy<Value = &'static [u8]> {
    prop::collection::vec(any::<u8>(), 0..max).prop_map(leak)
}

fn method() -> impl Strategy<Value = Method<'static>> {
    prop_oneof![
        Just(Method::Ping),
        (node_id(), want()).prop_map(|(target, want)| Method::FindNode { target, want }),
        (any::<[u8; 20]>(), want(), any::<bool>(), any::<bool>()).prop_map(
            |(h, want, scrape, noseed)| {
                Method::GetPeers {
                    info_hash: InfoHash(h),
                    want,
                    scrape,
                    noseed,
                }
            }
        ),
        (
            any::<[u8; 20]>(),
            any::<u16>(),
            any::<bool>(),
            bytes(16),
            any::<bool>()
        )
            .prop_map(
                |(h, port, implied_port, token, seed)| Method::AnnouncePeer {
                    info_hash: InfoHash(h),
                    port,
                    implied_port,
                    token,
                    seed,
                }
            ),
        (node_id(), want()).prop_map(|(target, want)| Method::SampleInfohashes { target, want }),
        "[a-z_]{1,12}"
            .prop_filter("known names", |n| {
                ![
                    "ping",
                    "find_node",
                    "get_peers",
                    "announce_peer",
                    "sample_infohashes",
                ]
                .contains(&n.as_str())
            })
            .prop_map(|n| Method::Unknown(leak(n.into_bytes()))),
    ]
}

fn response() -> impl Strategy<Value = Response<'static>> {
    let nodes = prop::collection::vec((node_id(), addr()), 0..6).prop_map(|v| {
        let infos: Vec<NodeInfo> = v
            .into_iter()
            .map(|(id, addr)| NodeInfo { id, addr })
            .collect();
        (
            leak(encode_nodes(&infos, false)),
            leak(encode_nodes(&infos, true)),
        )
    });
    (
        node_id(),
        nodes,
        prop::collection::vec(addr(), 0..6),
        prop::option::of(bytes(12)),
        prop::collection::vec(any::<[u8; 20]>(), 0..5),
        prop::option::of(any::<u32>()),
        prop::option::of(any::<u32>()),
        prop::option::of(any::<u8>()),
    )
        .prop_map(
            |(id, (n4, n6), values, token, samples, interval, num, bloom)| {
                let bloom: Option<&'static [u8; 256]> =
                    bloom.map(|b| leak(vec![b; 256]).try_into().unwrap());
                Response {
                    id,
                    nodes: CompactNodes::v4(n4).unwrap(),
                    nodes6: CompactNodes::v6(n6).unwrap(),
                    values,
                    token,
                    samples: Samples::new(leak(samples.concat())).unwrap(),
                    interval,
                    num,
                    seeds_bloom: bloom,
                    peers_bloom: bloom,
                }
            },
        )
}

fn message() -> impl Strategy<Value = Message<'static>> {
    let body = prop_oneof![
        (node_id(), method()).prop_map(|(id, method)| Body::Query(Query { id, method })),
        response().prop_map(Body::Response),
        (any::<i64>(), bytes(24))
            .prop_map(|(code, message)| Body::Error(ErrorBody { code, message })),
    ];
    (
        bytes(9),
        prop::option::of(bytes(6)),
        prop::option::of(addr()),
        any::<bool>(),
        body,
    )
        .prop_map(|(transaction_id, version, ip, read_only, body)| Message {
            transaction_id,
            version,
            ip,
            read_only,
            body,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn encode_parse_roundtrip(m in message()) {
        let bytes = m.encode();
        prop_assert_eq!(parse(&bytes).unwrap(), m);
        // Canonical: the decoder's strict mode accepts it.
        prop_assert!(lodestar_bencode::decode_strict(&bytes).is_ok());
    }

    /// Whatever parses, re-encodes to something that parses to the same
    /// message (lenient extras are dropped once, then stable).
    #[test]
    fn arbitrary_packets_are_stable(packet in prop::collection::vec(any::<u8>(), 0..200)) {
        if let Ok(m) = parse(&packet) {
            let again = m.encode();
            prop_assert_eq!(parse(&again).unwrap(), m);
        }
    }

    #[test]
    fn corrupted_messages_never_panic(
        m in message(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..6),
    ) {
        let mut bytes = m.encode();
        for (idx, b) in edits {
            let i = idx.index(bytes.len());
            bytes[i] = b;
        }
        if let Ok(m) = parse(&bytes) {
            let again = m.encode();
            prop_assert_eq!(parse(&again).unwrap(), m);
        }
    }
}
