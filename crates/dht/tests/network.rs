//! The in-process DHT test network (architecture §17): real UDP sockets on
//! loopback, real nodes, short timers.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use lodestar_dht::node::{Config, Deny, Intensity, Node};
use lodestar_dht::routing::{K, prefix_len};

fn config(bootstrap: Vec<String>) -> Config {
    Config {
        bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
        virtual_ids: 1,
        intensity: Intensity::Aggressive,
        bootstrap,
        state_path: None,
        sample_start_delay: Some(Duration::ZERO),
        allow_private: true,
        // Generous for loaded CI runners: a timeout only slows convergence.
        query_timeout: Duration::from_secs(1),
        tick: Duration::from_millis(50),
        save_every: Duration::from_secs(3600),
        read_only: false,
        harvest: Default::default(),
        deny: None,
        paused: false,
        restart: None,
        responder_hints: false,
        target_prefixes: false,
        utp: None,
    }
}

async fn wait_for(mut cond: impl AsyncFnMut() -> bool, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nodes_find_each_other_through_one_bootstrap_node() {
    let seed = Node::start(config(vec![]), None).await.unwrap();
    let seed_addr = seed.local_addrs()[0].to_string();
    let mut nodes = Vec::new();
    for _ in 0..40 {
        nodes.push(
            Node::start(config(vec![seed_addr.clone()]), None)
                .await
                .unwrap(),
        );
    }
    // Kademlia correctness: each node's 8 true closest nodes are all known,
    // except where BEP 5's "keep the old good nodes" rule leaves one out
    // because its bucket is already full. (That only happens in tiny
    // networks, where the closest nodes spread into shallow buckets; in the
    // real DHT they sit in the sparse, splittable deep buckets.)
    let mut all_ids = vec![seed.snapshot().await.unwrap().ids[0]];
    for n in &nodes {
        all_ids.push(n.snapshot().await.unwrap().ids[0]);
    }
    let missing = async || {
        let mut out = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            let s = n.snapshot().await.unwrap();
            let me = s.ids[0];
            let mut others: Vec<_> = all_ids.iter().filter(|x| **x != me).copied().collect();
            others.sort_by_key(|x| x.distance(&me));
            let known: HashSet<_> = s.nodes.iter().map(|n| n.id).collect();
            let bucket_full = |p: usize| {
                s.nodes
                    .iter()
                    .filter(|n| prefix_len(&me, &n.id) == p)
                    .count()
                    >= K
            };
            for x in others.iter().take(K) {
                if !known.contains(x) && !bucket_full(prefix_len(&me, x)) {
                    out.push((i, prefix_len(&me, x), s.table_sizes[0]));
                }
            }
        }
        out
    };
    let converged = wait_for(async || missing().await.is_empty(), Duration::from_secs(30)).await;
    assert!(
        converged,
        "true closest nodes missing from non-full buckets (node, prefix, table size): {:?}",
        missing().await
    );
    let seed_stats = seed.stats();
    assert!(seed_stats.queries_in > 0 && seed_stats.packets_out > 0);
    for n in &nodes {
        let s = n.stats();
        assert_eq!(s.malformed_in, 0);
        assert_eq!(s.spoofed, 0);
        assert!(s.reply_ratio() > 0.5, "reply ratio {:.2}", s.reply_ratio());
    }
    for n in nodes {
        n.shutdown().await.unwrap();
    }
    seed.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saved_state_rejoins_without_bootstrap_routers() {
    let dir = std::env::temp_dir().join(format!("lodestar-dht-net-{}", std::process::id()));
    let state = dir.join("state.bin");
    let seed = Node::start(config(vec![]), None).await.unwrap();
    let seed_addr = seed.local_addrs()[0].to_string();
    let mut others = Vec::new();
    for _ in 0..10 {
        others.push(
            Node::start(config(vec![seed_addr.clone()]), None)
                .await
                .unwrap(),
        );
    }
    let mut cfg = config(vec![seed_addr.clone()]);
    cfg.state_path = Some(state.clone());
    let node = Node::start(cfg.clone(), None).await.unwrap();
    assert!(
        wait_for(
            async || node.snapshot().await.unwrap().table_sizes[0] >= 5,
            Duration::from_secs(10)
        )
        .await
    );
    let ids = node.snapshot().await.unwrap().ids;
    node.shutdown().await.unwrap();
    assert!(state.exists(), "state saved on shutdown");

    // Restart with no bootstrap at all: the saved nodes are enough, and the
    // node keeps its identity.
    cfg.bootstrap.clear();
    let node = Node::start(cfg, None).await.unwrap();
    assert!(
        wait_for(
            async || node.snapshot().await.unwrap().table_sizes[0] >= 5,
            Duration::from_secs(10)
        )
        .await
    );
    assert_eq!(node.snapshot().await.unwrap().ids, ids);
    node.shutdown().await.unwrap();
    for n in others {
        n.shutdown().await.unwrap();
    }
    seed.shutdown().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn read_only_node_does_not_answer() {
    let mut cfg = config(vec![]);
    cfg.read_only = true;
    let ro = Node::start(cfg, None).await.unwrap();
    let ro_addr = ro.local_addrs()[0].to_string();
    let asker = Node::start(config(vec![ro_addr]), None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(ro.stats().queries_in > 0);
    assert_eq!(ro.stats().packets_out, 0);
    assert_eq!(asker.snapshot().await.unwrap().table_sizes[0], 0);
    asker.shutdown().await.unwrap();
    ro.shutdown().await.unwrap();
}

#[tokio::test]
async fn denied_addresses_are_never_contacted_or_answered() {
    let seed = Node::start(config(vec![]), None).await.unwrap();
    let seed_addr = seed.local_addrs()[0].to_string();
    let mut cfg = config(vec![seed_addr]);
    // Loopback stands in for a blocklisted range.
    cfg.deny = Some(Deny(Arc::new(|ip| ip.is_loopback())));
    let denying = Node::start(cfg, None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(denying.stats().denied > 0);
    assert_eq!(denying.stats().packets_out, 0);
    assert_eq!(seed.stats().packets_in, 0);
    assert_eq!(denying.snapshot().await.unwrap().table_sizes[0], 0);
    denying.shutdown().await.unwrap();
    seed.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejects_bad_bind_configs() {
    let mut cfg = config(vec![]);
    cfg.bind = vec![];
    assert!(Node::start(cfg.clone(), None).await.is_err());
    cfg.bind = vec![
        SocketAddr::from(([127, 0, 0, 1], 0)),
        SocketAddr::from(([127, 0, 0, 1], 0)),
    ];
    assert!(Node::start(cfg, None).await.is_err());
}
