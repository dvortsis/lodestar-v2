//! X-vpn-bind: a node crawling through one network interface pins its
//! sockets to it, so a missing interface fails its start (fail closed),
//! and a node pinned to loopback still crawls loopback. Linux only: it's
//! where an unprivileged test can name interfaces that exist (`lo`) and
//! don't. Windows binds the interface's address instead, which
//! `lodestar_network_policy::iface`'s own tests cover.
#![cfg(target_os = "linux")]

use std::net::SocketAddr;
use std::time::Duration;

use lodestar_dht::node::{Config, Intensity, Node};
use lodestar_network_policy::iface::{self, Guard, Interface};

mod wait;
use wait::{DEADLINE, wait_for};

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
        hint_first: false,
        lookup_end: false,
        soft_timeout: false,
        soft_timeout_after: lodestar_dht::node::SOFT_TIMEOUT,
        preset_slots: false,
        target_prefixes: false,
        utp: None,
        shard: None,
        sample_interval_scale: 1,
        sampler_cooldown_split: false,
        sample_backoff_escalate: false,
        guard: None,
    }
}

#[tokio::test]
async fn a_missing_interface_fails_the_start_and_binds_nothing() {
    let mut cfg = config(vec![]);
    cfg.bind = vec![
        SocketAddr::from(([0, 0, 0, 0], 0)),
        SocketAddr::from(([0u16; 8], 0)),
    ];
    let guard = Guard::new(Interface::parse("lsnone0").unwrap());
    assert!(!guard.state().is_up());
    cfg.guard = Some(guard);
    let before = iface::pins();
    let e = Node::start(cfg, None).await.unwrap_err();
    assert_eq!(
        e.raw_os_error(),
        Some(19),
        "ENODEV, not an unpinned bind: {e}"
    );
    assert!(iface::pins() > before, "the socket went through the pin");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nodes_pinned_to_loopback_still_find_each_other() {
    let lo = || Some(Guard::new(Interface::parse("lo").unwrap()));
    let mut seed_cfg = config(vec![]);
    seed_cfg.guard = lo();
    let seed = Node::start(seed_cfg, None).await.unwrap();
    // A name among the entries: skipped, never looked up (the system
    // resolver would ask outside the interface). The seed's literal
    // address is used, and the built-in router addresses join it.
    let mut cfg = config(vec![
        "localhost:6881".to_owned(),
        seed.local_addrs()[0].to_string(),
    ]);
    cfg.guard = lo();
    // Every other test here bootstraps from nothing, so no lookup can
    // land between the two reads but this test's own.
    let looked_up = lodestar_dht::node::lookups();
    let node = Node::start(cfg, None).await.unwrap();
    let joined = wait_for(
        async || node.snapshot().await.is_some_and(|s| !s.nodes.is_empty()),
        DEADLINE,
    )
    .await;
    assert!(joined, "a node pinned to lo didn't reach the seed on lo");
    assert_eq!(
        lodestar_dht::node::lookups(),
        looked_up,
        "no name went to the system resolver"
    );
    node.shutdown().await.unwrap();
    // The control: without an interface the same name is looked up.
    let plain = Node::start(config(vec!["localhost:6881".to_owned()]), None)
        .await
        .unwrap();
    // The actor looks names up once it runs, after `start` returns.
    let looked = wait_for(async || lodestar_dht::node::lookups() > looked_up, DEADLINE).await;
    assert!(looked, "the control never looked the name up");
    plain.shutdown().await.unwrap();
    seed.shutdown().await.unwrap();
}

/// The crawl's uTP rides the DHT's sockets (CR-4's shared port), so
/// pinning those pins it: a pinned node's uTP reaches a loopback peer from
/// the DHT's own port. After the node stops the port is detached (sends
/// refused), and the next node, a rebind after the interface came back,
/// attaches its fresh sockets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn utp_leaves_from_the_pinned_dht_socket_and_follows_a_rebind() {
    use lodestar_utp::{SharedPort, UtpSocket};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::timeout;

    let port = SharedPort::new(lodestar_utp::Config::default());
    let pinned = || {
        let mut cfg = config(vec![]);
        cfg.guard = Some(Guard::new(Interface::parse("lo").unwrap()));
        cfg.utp = Some(port.clone());
        cfg
    };
    let peer = UtpSocket::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let peer_addr = peer.local_addr().unwrap();
    let mut listener = peer.listen(4);
    let accept = tokio::spawn(async move {
        let mut froms = Vec::new();
        for _ in 0..2 {
            let mut s = listener.accept().await?;
            froms.push(s.peer_addr());
            let mut b = [0u8; 2];
            s.read_exact(&mut b).await?;
        }
        std::io::Result::Ok(froms)
    });
    let send = async || {
        let mut out = timeout(DEADLINE, port.socket().connect(peer_addr))
            .await
            .expect("connect in time")
            .expect("connected");
        out.write_all(b"hi").await.unwrap();
        out.flush().await.unwrap();
        out
    };

    let first = Node::start(pinned(), None).await.unwrap();
    let first_addr = first.local_addrs()[0];
    let _a = send().await;
    first.shutdown().await.unwrap();
    assert!(!port.attached(), "detached with its node");

    let second = Node::start(pinned(), None).await.unwrap();
    let second_addr = second.local_addrs()[0];
    let _b = send().await;
    let froms = timeout(DEADLINE, accept).await.unwrap().unwrap().unwrap();
    assert_eq!(
        froms,
        [first_addr, second_addr],
        "from each node's own pinned port"
    );
    second.shutdown().await.unwrap();
}
