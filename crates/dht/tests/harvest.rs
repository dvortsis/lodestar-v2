//! Harvest strategies against real nodes on loopback, driven partly by a
//! raw KRPC client so the passive paths can be exercised precisely.

use std::net::SocketAddr;
use std::time::Duration;

use lodestar_dht::NodeId;
use lodestar_dht::krpc::{self, Body, Message, Method, Query};
use lodestar_dht::node::{Config, Harvest, Intensity, Node, Source};
use lodestar_metainfo::InfoHash;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

fn config(bootstrap: Vec<String>) -> Config {
    Config {
        bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
        virtual_ids: 1,
        intensity: Intensity::Aggressive,
        bootstrap,
        state_path: None,
        sample_start_delay: Some(Duration::ZERO),
        allow_private: true,
        query_timeout: Duration::from_secs(1),
        tick: Duration::from_millis(50),
        save_every: Duration::from_secs(3600),
        read_only: false,
        harvest: Default::default(),
        deny: None,
        paused: false,
        restart: None,
    }
}

/// A bare KRPC client: sends one query and waits for its reply.
struct Client {
    socket: UdpSocket,
    id: NodeId,
    tid: u16,
}

impl Client {
    async fn new() -> Client {
        Client {
            socket: UdpSocket::bind("127.0.0.1:0").await.unwrap(),
            id: NodeId([0xcc; 20]),
            tid: 0,
        }
    }

    fn addr(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    /// Sends a query and returns the raw reply bytes.
    async fn ask(&mut self, to: SocketAddr, method: Method<'_>) -> Vec<u8> {
        self.tid += 1;
        let tid = self.tid.to_be_bytes();
        let msg = Message {
            transaction_id: &tid,
            version: None,
            ip: None,
            read_only: true,
            body: Body::Query(Query {
                id: self.id,
                method,
            }),
        };
        self.socket.send_to(&msg.encode(), to).await.unwrap();
        let mut buf = vec![0u8; 2048];
        loop {
            let (n, from) =
                tokio::time::timeout(Duration::from_secs(2), self.socket.recv_from(&mut buf))
                    .await
                    .expect("reply in time")
                    .unwrap();
            let reply = buf[..n].to_vec();
            let parsed = krpc::parse(&reply).unwrap();
            // Skip the node's own queries (e.g. verification pings).
            if from == to && parsed.transaction_id == tid {
                return reply;
            }
        }
    }
}

fn hash(i: u8) -> InfoHash {
    let mut h = [i; 20];
    h[0] = 0xab;
    InfoHash(h)
}

async fn next_harvest(rx: &mut mpsc::Receiver<Harvest>) -> Harvest {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("harvest in time")
        .expect("channel open")
}

#[tokio::test]
async fn passive_get_peers_and_announce() {
    let (tx, mut rx) = mpsc::channel(64);
    let node = Node::start(config(vec![]), Some(tx)).await.unwrap();
    let addr = node.local_addrs()[0];
    let mut client = Client::new().await;
    let h = hash(1);
    let get_peers = Method::GetPeers {
        info_hash: h,
        want: Default::default(),
        scrape: false,
        noseed: false,
    };

    let reply = client.ask(addr, get_peers.clone()).await;
    let Body::Response(r) = krpc::parse(&reply).unwrap().body else {
        panic!("expected a response")
    };
    let token = r.token.expect("get_peers reply carries a token").to_vec();
    assert!(r.values.is_empty());
    assert_eq!(
        next_harvest(&mut rx).await,
        Harvest {
            info_hash: h,
            source: Source::GetPeers,
            peers: vec![]
        }
    );
    // The same hash again is deduplicated.
    client.ask(addr, get_peers.clone()).await;

    // A bad token is refused and harvests nothing.
    let bad = client
        .ask(
            addr,
            Method::AnnouncePeer {
                info_hash: h,
                port: 7000,
                implied_port: false,
                token: b"nope",
                seed: false,
            },
        )
        .await;
    assert!(matches!(krpc::parse(&bad).unwrap().body, Body::Error(_)));

    // A valid announce harvests the hash with its live peer, implied port.
    let ok = client
        .ask(
            addr,
            Method::AnnouncePeer {
                info_hash: h,
                port: 1,
                implied_port: true,
                token: &token,
                seed: false,
            },
        )
        .await;
    assert!(matches!(krpc::parse(&ok).unwrap().body, Body::Response(_)));
    let announced = next_harvest(&mut rx).await;
    assert_eq!(announced.source, Source::Announce);
    assert_eq!(announced.peers, vec![client.addr()]);

    // The peer is now served to others.
    let reply = client.ask(addr, get_peers).await;
    let Body::Response(r) = krpc::parse(&reply).unwrap().body else {
        panic!()
    };
    assert_eq!(r.values, vec![client.addr()]);

    let s = node.stats();
    assert_eq!(s.harvested_get_peers, 1);
    assert_eq!(s.harvested_announce, 1);
    assert_eq!(node.snapshot().await.unwrap().stored_peer_hashes, 1);
    node.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bep51_sampling_harvests_hashes_other_nodes_hold() {
    let seed = Node::start(config(vec![]), None).await.unwrap();
    let seed_addr = seed.local_addrs()[0].to_string();
    let mut others = Vec::new();
    for _ in 0..8 {
        others.push(
            Node::start(config(vec![seed_addr.clone()]), None)
                .await
                .unwrap(),
        );
    }
    // One node learns 15 hashes from passive get_peers traffic; BEP 51
    // replies offer recently seen hashes.
    let holder = others[3].local_addrs()[0];
    let mut client = Client::new().await;
    for i in 0..15 {
        client
            .ask(
                holder,
                Method::GetPeers {
                    info_hash: hash(i),
                    want: Default::default(),
                    scrape: false,
                    noseed: false,
                },
            )
            .await;
    }

    let (tx, mut rx) = mpsc::channel(256);
    let crawler = Node::start(config(vec![seed_addr]), Some(tx))
        .await
        .unwrap();
    let mut got = std::collections::HashSet::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while got.len() < 15 && tokio::time::Instant::now() < deadline {
        if let Ok(Some(h)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            assert_eq!(h.source, Source::Sample);
            assert!(h.peers.is_empty());
            assert!(got.insert(h.info_hash), "sampled hashes are deduplicated");
        }
    }
    let want: std::collections::HashSet<_> = (0..15).map(hash).collect();
    assert!(
        want.is_subset(&got),
        "missing {} hashes",
        want.difference(&got).count()
    );
    let s = crawler.stats();
    assert!(s.samples_sent > 0);
    assert_eq!(s.harvested_sample as usize, got.len());

    crawler.shutdown().await.unwrap();
    for n in others {
        n.shutdown().await.unwrap();
    }
    seed.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn get_peers_lookup_finds_announced_peers() {
    let seed = Node::start(config(vec![]), None).await.unwrap();
    let seed_addr = seed.local_addrs()[0].to_string();
    let mut others = Vec::new();
    for _ in 0..5 {
        others.push(
            Node::start(config(vec![seed_addr.clone()]), None)
                .await
                .unwrap(),
        );
    }
    // Announce a peer to one node.
    let target = others[2].local_addrs()[0];
    let mut client = Client::new().await;
    let h = hash(99);
    let reply = client
        .ask(
            target,
            Method::GetPeers {
                info_hash: h,
                want: Default::default(),
                scrape: false,
                noseed: false,
            },
        )
        .await;
    let Body::Response(r) = krpc::parse(&reply).unwrap().body else {
        panic!()
    };
    let token = r.token.unwrap().to_vec();
    client
        .ask(
            target,
            Method::AnnouncePeer {
                info_hash: h,
                port: 51413,
                implied_port: false,
                token: &token,
                seed: true,
            },
        )
        .await;

    let mut cfg = config(vec![seed_addr]);
    cfg.harvest.sample = false;
    let (tx, mut rx) = mpsc::channel(16);
    let crawler = Node::start(cfg, Some(tx)).await.unwrap();
    // Let it join, then ask.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(crawler.get_peers(h));
    let found = loop {
        let harvest = next_harvest(&mut rx).await;
        if harvest.source == Source::Lookup {
            break harvest;
        }
    };
    assert_eq!(found.info_hash, h);
    assert_eq!(found.peers, vec![SocketAddr::from(([127, 0, 0, 1], 51413))]);
    assert_eq!(crawler.stats().lookups_completed, 1);

    crawler.shutdown().await.unwrap();
    for n in others {
        n.shutdown().await.unwrap();
    }
    seed.shutdown().await.unwrap();
}
