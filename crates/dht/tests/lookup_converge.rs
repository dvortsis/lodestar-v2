//! X-dht-lookup-converge's reproducer on real loopback nodes: a 50-node
//! network that has only just joined, 18 swarms announced to their true
//! closest nodes, and a fresh node asking for all 18 the moment it
//! starts. Before the fix about one round in three had empty lookups:
//! all 18 when they started before the bootstrap answered, a few when
//! the young network's nodes didn't know the far keyspace.
//!
//! A measurement on real time (about 45 s), so it's ignored by default;
//! the deterministic tests are the actor's `join_tests`.
//! `cargo test -p lodestar-dht --test lookup_converge -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use lodestar_dht::NodeId;
use lodestar_dht::krpc::{self, Body, Message, Method, Query};
use lodestar_dht::node::{Config, HarvestConfig, Intensity, Node, Source};
use lodestar_metainfo::InfoHash;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

const ROUNDS: u8 = 15;
const NODES: usize = 50;
const HASHES: u8 = 18;
/// Every wait's deadline (standards §2: at least 10 s).
const DEADLINE: Duration = Duration::from_secs(20);

fn loopback() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

/// A network node that answers and stores, but walks nothing itself.
fn net_config(bootstrap: Vec<String>) -> Config {
    Config {
        bind: vec![loopback()],
        virtual_ids: 1,
        intensity: Intensity::Aggressive,
        bootstrap,
        sample_start_delay: Some(Duration::ZERO),
        allow_private: true,
        query_timeout: Duration::from_secs(1),
        tick: Duration::from_millis(50),
        harvest: HarvestConfig {
            sample: false,
            passive: true,
        },
        ..Config::default()
    }
}

/// One query from a bare client, and its reply.
async fn ask(sock: &UdpSocket, tid: &mut u16, to: SocketAddr, method: Method<'_>) -> Vec<u8> {
    *tid = tid.wrapping_add(1);
    let t = tid.to_be_bytes();
    let msg = Message {
        transaction_id: &t,
        version: None,
        ip: None,
        read_only: true,
        body: Body::Query(Query {
            id: NodeId([0xcc; 20]),
            method,
        }),
    };
    sock.send_to(&msg.encode(), to).await.expect("send");
    let mut buf = vec![0; 2048];
    loop {
        let (n, from) = tokio::time::timeout(DEADLINE, sock.recv_from(&mut buf))
            .await
            .expect("a reply within the deadline")
            .expect("recv");
        if from == to && krpc::parse(&buf[..n]).is_ok_and(|m| m.transaction_id == t) {
            return buf[..n].to_vec();
        }
    }
}

/// Starts the network and waits until every node knows at least 8.
async fn network() -> Vec<Node> {
    let first = Node::start(net_config(vec![]), None).await.expect("bind");
    let at = first.local_addrs()[0].to_string();
    let mut nodes = vec![first];
    for _ in 1..NODES {
        nodes.push(
            Node::start(net_config(vec![at.clone()]), None)
                .await
                .expect("bind"),
        );
    }
    let end = tokio::time::Instant::now() + DEADLINE;
    loop {
        let mut short = 0;
        for n in &nodes {
            if n.snapshot().await.expect("running").table_sizes[0] < 8 {
                short += 1;
            }
        }
        if short == 0 {
            return nodes;
        }
        assert!(
            tokio::time::Instant::now() < end,
            "{short} nodes still know fewer than 8"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: about 45 s of real time"]
async fn a_fresh_node_finds_every_announced_swarm() {
    let mut empty_rounds = 0;
    for round in 0..ROUNDS {
        let nodes = network().await;
        let at = nodes[0].local_addrs()[0].to_string();
        let mut ids = Vec::new();
        for n in &nodes {
            ids.push((
                n.snapshot().await.expect("running").ids[0],
                n.local_addrs()[0],
            ));
        }
        // Announce each swarm to its 8 true closest nodes.
        let sock = UdpSocket::bind(loopback()).await.expect("bind");
        let mut tid = 0u16;
        let mut hashes = Vec::new();
        for i in 0..HASHES {
            let mut h = [0u8; 20];
            h[0] = round.wrapping_mul(37).wrapping_add(i.wrapping_mul(14));
            h[1] = i;
            h[2] = round;
            let h = InfoHash(h);
            let mut by = ids.clone();
            by.sort_by_key(|(id, _)| id.distance(&NodeId(h.0)));
            for (_, to) in by.iter().take(8) {
                let get = Method::GetPeers {
                    info_hash: h,
                    want: Default::default(),
                    scrape: false,
                    noseed: false,
                };
                let reply = ask(&sock, &mut tid, *to, get).await;
                let Body::Response(r) = krpc::parse(&reply).expect("KRPC").body else {
                    panic!("get_peers to {to} wasn't answered");
                };
                let token = r.token.expect("a token").to_vec();
                let announce = Method::AnnouncePeer {
                    info_hash: h,
                    port: 40000 + u16::from(i),
                    implied_port: false,
                    token: &token,
                    seed: true,
                };
                ask(&sock, &mut tid, *to, announce).await;
            }
            hashes.push(h);
        }
        // A fresh node asks for every swarm at once, as the pool's
        // restored queue does at a start.
        let (tx, mut rx) = mpsc::channel(4096);
        let crawler = Node::start(
            Config {
                bind: vec![loopback()],
                bootstrap: vec![at],
                allow_private: true,
                sample_start_delay: Some(Duration::ZERO),
                harvest: HarvestConfig {
                    sample: false,
                    passive: true,
                },
                ..Config::default()
            },
            Some(tx),
        )
        .await
        .expect("bind");
        for h in &hashes {
            assert!(crawler.get_peers(*h), "queued");
        }
        let mut got = BTreeMap::new();
        let end = tokio::time::Instant::now() + DEADLINE;
        while got.len() < hashes.len() {
            let left = end.saturating_duration_since(tokio::time::Instant::now());
            let h = tokio::time::timeout(left, rx.recv())
                .await
                .unwrap_or_else(|_| panic!("round {round}: {} of 18 answered", got.len()))
                .expect("node running");
            if h.source == Source::Lookup {
                got.insert(h.info_hash.0, h.peers.len());
            }
        }
        let empty: Vec<String> = got
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(h, _)| format!("{:02x}{:02x}", h[0], h[1]))
            .collect();
        println!("round {round}: empty lookups {empty:?}");
        if !empty.is_empty() {
            empty_rounds += 1;
        }
        crawler.shutdown().await.expect("stops");
        for n in nodes {
            n.shutdown().await.expect("stops");
        }
    }
    assert_eq!(empty_rounds, 0, "rounds with an empty lookup");
}
