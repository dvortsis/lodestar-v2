//! The DHT the crawler joins: real `lodestar_dht` nodes on loopback UDP
//! (as `crates/dht/tests/network.rs` runs them), and a bare KRPC client
//! that announces the fake peers the way a BitTorrent client does: a
//! `get_peers` for the token, then `announce_peer`, to the nodes closest
//! to each info hash.

use std::net::SocketAddr;
use std::time::Duration;

use lodestar_dht::NodeId;
use lodestar_dht::krpc::{self, Body, Message, Method, Query};
use lodestar_dht::node::{Config, Intensity, Node};
use lodestar_metainfo::InfoHash;
use tokio::net::UdpSocket;

use super::wait::until;

/// A BitTorrent client announces to the K closest nodes (BEP 5).
pub const ANNOUNCE_TO: usize = 8;

fn loopback() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

/// A network node: it answers, stores announces and lists what it has
/// seen in BEP 51 samples, but walks nothing itself, so the only sampler
/// is the crawler.
fn node_config(bootstrap: Vec<String>) -> Config {
    Config {
        bind: vec![loopback()],
        virtual_ids: 1,
        intensity: Intensity::Aggressive,
        bootstrap,
        state_path: None,
        sample_start_delay: Some(Duration::ZERO),
        allow_private: true,
        // Generous for loaded CI runners: a timeout only slows joining.
        query_timeout: Duration::from_secs(1),
        tick: Duration::from_millis(50),
        harvest: lodestar_dht::node::HarvestConfig {
            sample: false,
            passive: true,
        },
        ..Config::default()
    }
}

pub struct Network {
    nodes: Vec<Node>,
    ids: Vec<(NodeId, SocketAddr)>,
}

impl Network {
    /// `n` nodes, all bootstrapped from the first, and joined: every node
    /// knows at least `min(8, n - 1)` others.
    pub async fn start(n: usize) -> Network {
        let first = Node::start(node_config(vec![]), None).await.expect("bind");
        let at = first.local_addrs()[0].to_string();
        let mut nodes = vec![first];
        for _ in 1..n {
            nodes.push(
                Node::start(node_config(vec![at.clone()]), None)
                    .await
                    .expect("bind"),
            );
        }
        let want = (n - 1).min(8);
        let nodes_ref = &nodes;
        until("the network to join", Duration::from_secs(20), async || {
            let mut short = 0;
            for node in nodes_ref {
                let s = node.snapshot().await.expect("running");
                if s.table_sizes[0] < want {
                    short += 1;
                }
            }
            (short == 0)
                .then_some(())
                .ok_or(format!("{short} nodes know < {want}"))
        })
        .await;
        let mut ids = Vec::new();
        for node in &nodes {
            let s = node.snapshot().await.expect("running");
            ids.push((s.ids[0], node.local_addrs()[0]));
        }
        Network { nodes, ids }
    }

    /// Where a new node, or a client's lookup, starts.
    pub fn bootstrap(&self) -> SocketAddr {
        self.nodes[0].local_addrs()[0]
    }

    /// The `k` nodes closest to `hash` by XOR distance (BEP 5): where a
    /// client's lookup would end, so where it announces.
    pub fn closest(&self, hash: InfoHash, k: usize) -> Vec<SocketAddr> {
        let target = NodeId(hash.0);
        let mut ids = self.ids.clone();
        ids.sort_by_key(|(id, _)| id.distance(&target));
        ids.into_iter().take(k).map(|(_, a)| a).collect()
    }

    /// Replies the network's nodes dropped to their rate limits, and
    /// queries of theirs that timed out.
    pub fn dropped_and_timeouts(&self) -> (u64, u64) {
        self.nodes.iter().fold((0, 0), |(d, t), n| {
            let s = n.stats();
            (d + s.replies_dropped, t + s.timeouts)
        })
    }

    pub async fn shutdown(self) {
        for node in self.nodes {
            node.shutdown().await.expect("no state to save");
        }
    }
}

/// A bare KRPC client: one query at a time, waiting for its reply.
pub struct Client {
    socket: UdpSocket,
    id: NodeId,
    tid: u16,
}

impl Client {
    pub async fn new(id: [u8; 20]) -> Client {
        Client {
            socket: UdpSocket::bind(loopback()).await.expect("bind"),
            id: NodeId(id),
            tid: 0,
        }
    }

    /// Sends `method` to `to` and returns the reply, tried up to three
    /// times (UDP).
    async fn ask(&mut self, to: SocketAddr, method: Method<'_>) -> Vec<u8> {
        for _ in 0..3 {
            self.tid = self.tid.wrapping_add(1);
            let tid = self.tid.to_be_bytes();
            let msg = Message {
                transaction_id: &tid,
                version: None,
                ip: None,
                // A client, not a node: keep it out of routing tables.
                read_only: true,
                body: Body::Query(Query {
                    id: self.id,
                    method: method.clone(),
                }),
            };
            self.socket
                .send_to(&msg.encode(), to)
                .await
                .expect("send on loopback");
            let mut buf = vec![0u8; 2048];
            let reply = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let (n, from) = self.socket.recv_from(&mut buf).await?;
                    let ours =
                        krpc::parse(&buf[..n]).is_ok_and(|m| from == to && m.transaction_id == tid);
                    if ours {
                        return Ok::<_, std::io::Error>(buf[..n].to_vec());
                    }
                }
            })
            .await;
            if let Ok(Ok(reply)) = reply {
                return reply;
            }
        }
        panic!("no reply from {to}");
    }

    /// A `get_peers` for `hash`: the node sees the hash (a passive
    /// harvest) and hands out an announce token.
    async fn get_peers(&mut self, to: SocketAddr, hash: InfoHash) -> Vec<u8> {
        let reply = self
            .ask(
                to,
                Method::GetPeers {
                    info_hash: hash,
                    want: Default::default(),
                    scrape: false,
                    noseed: false,
                },
            )
            .await;
        match krpc::parse(&reply).expect("a KRPC reply").body {
            Body::Response(r) => r.token.expect("get_peers returns a token").to_vec(),
            other => panic!("get_peers to {to}: {other:?}"),
        }
    }

    /// Announces `peer_port` on 127.0.0.1 for `hash` to the nodes closest
    /// to it, each with the token its `get_peers` handed out.
    pub async fn announce(&mut self, net: &Network, hash: InfoHash, peer_port: u16) {
        for to in net.closest(hash, ANNOUNCE_TO) {
            let token = self.get_peers(to, hash).await;
            let reply = self
                .ask(
                    to,
                    Method::AnnouncePeer {
                        info_hash: hash,
                        port: peer_port,
                        implied_port: false,
                        token: &token,
                        seed: true,
                    },
                )
                .await;
            let body = krpc::parse(&reply).expect("a KRPC reply").body;
            assert!(
                matches!(body, Body::Response(_)),
                "announce to {to}: {body:?}"
            );
        }
    }

    /// Asks the closest nodes for peers of `hash` and announces nothing:
    /// the network sees the hash, but no peer has it.
    pub async fn ask_only(&mut self, net: &Network, hash: InfoHash) {
        for to in net.closest(hash, ANNOUNCE_TO) {
            self.get_peers(to, hash).await;
        }
    }
}
