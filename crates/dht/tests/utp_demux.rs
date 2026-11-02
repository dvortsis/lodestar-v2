//! CR-4's shared port (§5): with `utp` set, uTP datagrams on the DHT's
//! socket reach the uTP socket and its sends leave from the same port,
//! while KRPC is answered as before; without it, nothing changes.

use std::net::SocketAddr;
use std::time::Duration;

use lodestar_dht::node::{Config, Node};
use lodestar_utp::{SharedPort, UtpSocket};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(10);
const PING: &[u8] = b"d1:ad2:id20:aaaaaaaaaaaaaaaaaaaae1:q4:ping1:t2:aa1:y1:qe";

fn config(utp: Option<SharedPort>) -> Config {
    Config {
        bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
        bootstrap: vec![],
        state_path: None,
        allow_private: true,
        utp,
        ..Config::default()
    }
}

/// A KRPC ping to `node` gets a bencoded answer.
fn answers_ping(node: SocketAddr) -> bool {
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_read_timeout(Some(DEADLINE)).unwrap();
    udp.send_to(PING, node).unwrap();
    let mut buf = [0u8; 1500];
    matches!(udp.recv_from(&mut buf), Ok((n, from)) if from == node && buf[..n].starts_with(b"d1:"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn utp_shares_the_dht_port_and_krpc_still_works() {
    let port = SharedPort::new(lodestar_utp::Config::default());
    let node = Node::start(config(Some(port.clone())), None).await.unwrap();
    assert!(port.attached());
    let dht_addr = node.local_addrs()[0];

    let peer = UtpSocket::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let peer_addr = peer.local_addr().unwrap();
    let mut listener = peer.listen(4);
    let echo = tokio::spawn(async move {
        let mut s = listener.accept().await?;
        let from = s.peer_addr();
        let mut b = [0u8; 5];
        s.read_exact(&mut b).await?;
        s.write_all(&b).await?;
        s.flush().await?;
        std::io::Result::Ok(from)
    });
    let mut out = timeout(DEADLINE, port.socket().connect(peer_addr))
        .await
        .expect("connect in time")
        .expect("connected");
    out.write_all(b"hello").await.unwrap();
    out.flush().await.unwrap();
    let mut back = [0u8; 5];
    timeout(DEADLINE, out.read_exact(&mut back))
        .await
        .expect("echo in time")
        .unwrap();
    assert_eq!(&back, b"hello");
    let from = timeout(DEADLINE, echo).await.unwrap().unwrap().unwrap();
    assert_eq!(from, dht_addr, "uTP leaves from the DHT's port");
    assert!(node.stats().utp_in > 0);
    assert_eq!(node.stats().malformed_in, 0);

    let ping = tokio::task::spawn_blocking(move || answers_ping(dht_addr));
    assert!(
        ping.await.unwrap(),
        "KRPC still answered on the shared port"
    );
    node.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_utp_a_utp_datagram_is_malformed_krpc_as_before() {
    let node = Node::start(config(None), None).await.unwrap();
    let dht_addr = node.local_addrs()[0];
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    // A uTP SYN header (type 4, version 1).
    udp.send_to(&[0x41; 20], dht_addr).unwrap();
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while node.stats().malformed_in == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stats = node.stats();
    assert_eq!((stats.malformed_in, stats.utp_in), (1, 0));
    node.shutdown().await.unwrap();
}
