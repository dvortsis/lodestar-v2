//! BEP 33 scrapes against real nodes on loopback. Announcers bind distinct
//! loopback addresses (127.0.0.2, …), because the filters count IPs, not
//! ports.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use lodestar_dht::NodeId;
use lodestar_dht::bep33::Bloom;
use lodestar_dht::krpc::{self, Body, Message, Method, Query, Response};
use lodestar_dht::node::{Config, DhtEstimate, Intensity, Node};
use lodestar_metainfo::InfoHash;
use tokio::net::UdpSocket;

fn config(bootstrap: Vec<String>) -> Config {
    Config {
        bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
        virtual_ids: 1,
        intensity: Intensity::Aggressive,
        bootstrap,
        state_path: None,
        query_timeout: Duration::from_secs(1),
        tick: Duration::from_millis(50),
        save_every: Duration::from_secs(3600),
        read_only: false,
        harvest: Default::default(),
    }
}

/// A bare KRPC client on its own loopback address.
struct Client {
    socket: UdpSocket,
    tid: u16,
}

impl Client {
    async fn on(last_octet: u8) -> Client {
        let ip = Ipv4Addr::new(127, 0, 0, last_octet);
        Client {
            socket: UdpSocket::bind((ip, 0)).await.unwrap(),
            tid: 0,
        }
    }

    /// Sends a query and returns the raw reply.
    async fn ask(&mut self, to: SocketAddr, method: Method<'_>) -> Vec<u8> {
        self.tid += 1;
        let tid = self.tid.to_be_bytes();
        let msg = Message {
            transaction_id: &tid,
            version: None,
            ip: None,
            read_only: true,
            body: Body::Query(Query {
                id: NodeId([0xcc; 20]),
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
            if from == to && krpc::parse(&reply).unwrap().transaction_id == tid {
                return reply;
            }
        }
    }

    /// get_peers for a token, then announce_peer.
    async fn announce(&mut self, to: SocketAddr, h: InfoHash, seed: bool) {
        let reply = self.ask(to, get_peers(h, false, false)).await;
        let token = response(&reply).token.unwrap().to_vec();
        let ok = self
            .ask(
                to,
                Method::AnnouncePeer {
                    info_hash: h,
                    port: 6881,
                    implied_port: false,
                    token: &token,
                    seed,
                },
            )
            .await;
        assert!(matches!(krpc::parse(&ok).unwrap().body, Body::Response(_)));
    }
}

fn get_peers(info_hash: InfoHash, scrape: bool, noseed: bool) -> Method<'static> {
    Method::GetPeers {
        info_hash,
        want: Default::default(),
        scrape,
        noseed,
    }
}

fn response(raw: &[u8]) -> Response<'_> {
    match krpc::parse(raw).unwrap().body {
        Body::Response(r) => r,
        other => panic!("expected a response, got {other:?}"),
    }
}

fn hash(i: u8) -> InfoHash {
    let mut h = [i; 20];
    h[0] = 0x5c;
    InfoHash(h)
}

#[tokio::test]
async fn answers_scrapes_only_when_asked_and_only_for_stored_hashes() {
    let node = Node::start(config(vec![]), None).await.unwrap();
    let addr = node.local_addrs()[0];
    let h = hash(1);
    let mut seed = Client::on(2).await;
    let mut leech = Client::on(3).await;
    seed.announce(addr, h, true).await;
    leech.announce(addr, h, false).await;

    let mut asker = Client::on(4).await;
    let plain = asker.ask(addr, get_peers(h, false, false)).await;
    let r = response(&plain);
    assert!(r.seeds_bloom.is_none() && r.peers_bloom.is_none());
    assert_eq!(r.values.len(), 2);

    let scraped = asker.ask(addr, get_peers(h, true, false)).await;
    let r = response(&scraped);
    let sd = Bloom(*r.seeds_bloom.expect("BFsd"));
    let pe = Bloom(*r.peers_bloom.expect("BFpe"));
    assert_eq!((sd.count(), pe.count()), (1, 1));
    let mut expect = Bloom::default();
    expect.insert(IpAddr::from([127, 0, 0, 2]));
    assert_eq!(sd, expect, "BFsd holds the seed's IP");

    // noseed leaves the seed out of `values`.
    let r = asker.ask(addr, get_peers(h, false, true)).await;
    assert_eq!(
        response(&r).values,
        [SocketAddr::from(([127, 0, 0, 3], 6881))]
    );

    // A hash the node doesn't store gets no filters.
    let none = asker.ask(addr, get_peers(hash(2), true, false)).await;
    assert!(response(&none).seeds_bloom.is_none());
    node.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scrape_lookup_estimates_the_swarm() {
    let seed_node = Node::start(config(vec![]), None).await.unwrap();
    let seed_addr = seed_node.local_addrs()[0].to_string();
    let mut others = Vec::new();
    for _ in 0..6 {
        others.push(
            Node::start(config(vec![seed_addr.clone()]), None)
                .await
                .unwrap(),
        );
    }
    // 4 seeds and 7 leechers, each announcing to every node, as real
    // peers announce to the K closest.
    let h = hash(7);
    let addrs: Vec<SocketAddr> = others.iter().map(|n| n.local_addrs()[0]).collect();
    for i in 0..11u8 {
        let mut c = Client::on(10 + i).await;
        for a in &addrs {
            c.announce(*a, h, i < 4).await;
        }
    }

    let mut cfg = config(vec![seed_addr]);
    cfg.harvest.sample = false;
    let crawler = Node::start(cfg, None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let handle = crawler.handle();
    let est = tokio::time::timeout(Duration::from_secs(20), handle.scrape(h))
        .await
        .expect("scrape in time")
        .expect("scrape answered");
    assert_eq!((est.seeds, est.peers), (4, 7), "{est:?}");
    assert!(est.responders >= 1, "{est:?}");

    // A hash nobody stores: no responders, which means "unknown".
    let unknown = handle.scrape(hash(8)).await.expect("answered");
    assert_eq!(unknown, DhtEstimate::default());

    let s = crawler.stats();
    assert_eq!(s.scrapes_requested, 2);
    assert_eq!(s.scrapes_completed, 2);
    assert_eq!(s.scrapes_with_responders, 1);
    assert_eq!(s.lookups_completed, 0, "scrapes aren't harvests");

    crawler.shutdown().await.unwrap();
    assert!(handle.scrape(h).await.is_none(), "stopped node");
    for n in others {
        n.shutdown().await.unwrap();
    }
    seed_node.shutdown().await.unwrap();
}
