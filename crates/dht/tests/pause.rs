//! The crawler's pause (`NodeHandle::set_paused`): no sampling, lookups,
//! scrapes or harvests, while the node keeps answering other nodes; and
//! the count of unsolicited queries the network check reads.

use std::net::SocketAddr;
use std::time::Duration;

use lodestar_dht::NodeId;
use lodestar_dht::krpc::{self, Body, Message, Method, Query};
use lodestar_dht::node::{Config, Harvest, Intensity, Node, Source};
use lodestar_metainfo::InfoHash;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

mod wait;
use wait::{a_tick_ran, wait_for};

fn config(bootstrap: Vec<String>, paused: bool) -> Config {
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
        paused,
        ..Config::default()
    }
}

fn query(tid: u16, method: Method<'_>) -> Vec<u8> {
    let tid = tid.to_be_bytes();
    Message {
        transaction_id: &tid,
        version: None,
        ip: None,
        read_only: true,
        body: Body::Query(Query {
            id: NodeId([0xcc; 20]),
            method,
        }),
    }
    .encode()
}

/// Sends a ping from `socket` and waits for the node's answer.
async fn ping(socket: &UdpSocket, to: SocketAddr, tid: u16) -> bool {
    ask(socket, to, &query(tid, Method::Ping), tid).await
}

/// Sends `msg` (a query with transaction id `tid`) from `socket` and waits
/// for the node's answer to it.
async fn ask(socket: &UdpSocket, to: SocketAddr, msg: &[u8], tid: u16) -> bool {
    socket.send_to(msg, to).await.unwrap();
    let mut buf = vec![0u8; 2048];
    let deadline = tokio::time::Instant::now() + wait::DEADLINE;
    loop {
        let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await
        else {
            return false;
        };
        let Ok(m) = krpc::parse(&buf[..n]) else {
            continue;
        };
        if from == to && m.transaction_id == tid.to_be_bytes() {
            return matches!(m.body, Body::Response(_));
        }
    }
}

#[tokio::test]
async fn a_paused_node_answers_but_doesnt_crawl() {
    let b = Node::start(config(vec![], false), None).await.unwrap();
    let b_addr = b.local_addrs()[0].to_string();
    let (tx, mut harvests) = mpsc::channel::<Harvest>(64);
    let a = Node::start(config(vec![b_addr], true), Some(tx))
        .await
        .unwrap();
    let a_addr = a.local_addrs()[0];
    let handle = a.handle();
    assert!(a.snapshot().await.unwrap().paused);

    // Maintenance goes on: A bootstraps from B and answers queries.
    assert!(
        wait_for(async || a.stats().routing_nodes > 0, wait::DEADLINE).await,
        "the paused node didn't bootstrap from B within 10 s: {:?}",
        a.stats()
    );
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    assert!(
        ping(&client, a_addr, 1).await,
        "a paused node still answers"
    );

    // Nothing is sampled, looked up, scraped or harvested.
    let key = InfoHash([0x5a; 20]);
    assert!(a.get_peers(key));
    let get_peers = query(
        2,
        Method::GetPeers {
            info_hash: InfoHash([0x77; 20]),
            want: Default::default(),
            scrape: false,
            noseed: false,
        },
    );
    // Its answer comes after the query is handled, harvest or not.
    assert!(
        ask(&client, a_addr, &get_peers, 2).await,
        "a paused node still answers get_peers"
    );
    // The scrape's answer also orders the queued lookup (an earlier command)
    // before the tick that `a_tick_ran` waits for.
    assert_eq!(handle.scrape(key).await, None, "no scrape while paused");
    a_tick_ran(&a).await;
    let s = a.stats();
    assert_eq!(s.samples_sent, 0);
    assert_eq!(s.harvested_get_peers, 0);
    assert_eq!(s.lookups_completed, 0);
    assert!(harvests.try_recv().is_err(), "no harvest while paused");

    // Resumed: sampling starts, and the queued lookup runs.
    assert!(handle.set_paused(false).await);
    assert!(!a.snapshot().await.unwrap().paused);
    assert!(
        wait_for(async || a.stats().samples_sent > 0, wait::DEADLINE).await,
        "the resumed node sent no sample within 10 s: {:?}",
        a.stats()
    );
    let got = tokio::time::timeout(wait::DEADLINE, async {
        loop {
            let h = harvests.recv().await.unwrap();
            if h.source == Source::Lookup {
                return h;
            }
        }
    })
    .await
    .expect("the held lookup runs after resume");
    assert_eq!(got.info_hash, key);
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
}

#[tokio::test]
async fn only_queries_from_nodes_we_never_asked_count_as_unsolicited() {
    // The node bootstraps from `known`, so it has queried it.
    let known = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let node = Node::start(
        config(vec![known.local_addr().unwrap().to_string()], false),
        None,
    )
    .await
    .unwrap();
    let addr = node.local_addrs()[0];
    let mut buf = vec![0u8; 2048];
    let (_, from) = tokio::time::timeout(wait::DEADLINE, known.recv_from(&mut buf))
        .await
        .expect("the bootstrap query arrives")
        .unwrap();
    assert_eq!(from, addr);

    assert!(ping(&known, addr, 1).await);
    let s = node.stats();
    assert_eq!((s.queries_in, s.queries_in_unsolicited), (1, 0));

    let stranger = UdpSocket::bind("127.0.0.2:0").await;
    // Some sandboxes have only 127.0.0.1; a second port on it shares the
    // known IP, which is the conservative case and proves nothing here.
    let Ok(stranger) = stranger else { return };
    assert!(ping(&stranger, addr, 2).await);
    let s = node.stats();
    assert_eq!((s.queries_in, s.queries_in_unsolicited), (2, 1));
    // We answered it, which opens a NAT mapping to it: its next queries
    // prove nothing about our port, so they aren't unsolicited.
    for tid in 3..6 {
        assert!(ping(&stranger, addr, tid).await);
    }
    let s = node.stats();
    assert_eq!((s.queries_in, s.queries_in_unsolicited), (5, 1));
    node.shutdown().await.unwrap();
}
