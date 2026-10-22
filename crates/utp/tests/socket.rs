//! uTP sockets over real loopback UDP.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_utp::{Config, PacketSink, Protocol, UtpSocket, classify};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::time::{sleep, timeout};

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

/// A loopback socket. Every test peer shares 127.0.0.1, so the per-IP
/// cap is lifted (`per_ip_cap_is_enforced` covers it).
async fn bind_local() -> UtpSocket {
    UtpSocket::bind_with(
        localhost(),
        Config {
            max_per_ip: 1024,
            ..Config::default()
        },
    )
    .await
    .unwrap()
}

/// Waits until `cond` holds, for up to `secs`.
async fn eventually(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..secs * 50 {
        if cond() {
            return true;
        }
        sleep(Duration::from_millis(20)).await;
    }
    cond()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn echo_a_megabyte_each_way() {
    let server = bind_local().await;
    let mut listener = server.listen(8);
    let addr = server.local_addr().unwrap();
    tokio::spawn(async move {
        let mut s = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = s.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            s.write_all(&buf[..n]).await.unwrap();
        }
        s.shutdown().await.unwrap();
    });
    let client = bind_local().await;
    let stream = timeout(Duration::from_secs(5), client.connect(addr))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stream.peer_addr(), addr);
    let (mut rd, mut wr) = tokio::io::split(stream);
    let payload = data(1 << 20);
    let sent = payload.clone();
    let writer = tokio::spawn(async move {
        wr.write_all(&sent).await.unwrap();
        wr.shutdown().await.unwrap();
        wr
    });
    let mut got = Vec::new();
    timeout(Duration::from_secs(20), rd.read_to_end(&mut got))
        .await
        .unwrap()
        .unwrap();
    assert!(got == payload, "{} of {} bytes", got.len(), payload.len());
    drop(writer.await.unwrap());
    drop(rd);
    // Both sides close cleanly and unregister.
    assert!(
        eventually(10, || client.connections() == 0
            && server.connections() == 0)
        .await
    );
    let stats = client.stats();
    assert_eq!(stats.connects, 1);
    assert_eq!(stats.timeouts, 0);
    assert_eq!(server.stats().accepts, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_connections_at_once() {
    let server = bind_local().await;
    let mut listener = server.listen(64);
    let addr = server.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok(mut s) = listener.accept().await {
            tokio::spawn(async move {
                let mut id = [0u8; 2];
                s.read_exact(&mut id).await.unwrap();
                let reply = data(10_000 + usize::from(u16::from_be_bytes(id)));
                s.write_all(&reply).await.unwrap();
                s.shutdown().await.unwrap();
            });
        }
    });
    let client = bind_local().await;
    let mut tasks = Vec::new();
    for i in 0..100u16 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            let mut s = client.connect(addr).await.unwrap();
            s.write_all(&i.to_be_bytes()).await.unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).await.unwrap();
            assert_eq!(got, data(10_000 + usize::from(i)));
        }));
    }
    for t in tasks {
        timeout(Duration::from_secs(20), t).await.unwrap().unwrap();
    }
    assert!(eventually(10, || client.connections() == 0).await);
}

#[tokio::test]
async fn nobody_listening() {
    // A bound port without a listener ignores the SYN: connecting times
    // out (bounded by the caller), and the abandoned attempt unregisters.
    let quiet = bind_local().await;
    let client = bind_local().await;
    let r = timeout(
        Duration::from_millis(300),
        client.connect(quiet.local_addr().unwrap()),
    )
    .await;
    assert!(r.is_err(), "still waiting for a SYN-ACK");
    assert!(eventually(2, || client.connections() == 0).await);
    assert_eq!(
        quiet.stats().unknown,
        1,
        "{:?} / {:?}",
        quiet.stats(),
        client.stats()
    );
    assert_eq!(quiet.connections(), 0);
}

#[tokio::test]
async fn connection_cap_is_enforced() {
    let client = UtpSocket::bind_with(
        localhost(),
        Config {
            max_connections: 1,
            ..Config::default()
        },
    )
    .await
    .unwrap();
    let quiet = bind_local().await;
    let target = quiet.local_addr().unwrap();
    let c = client.clone();
    let pending = tokio::spawn(async move { c.connect(target).await.map(drop) });
    assert!(eventually(2, || client.connections() == 1).await);
    let err = client.connect(target).await.unwrap_err();
    assert!(err.to_string().contains("too many"), "{err}");
    pending.abort();
}

#[tokio::test]
async fn reset_reaches_the_peer() {
    let server = bind_local().await;
    let mut listener = server.listen(1);
    let addr = server.local_addr().unwrap();
    let client = bind_local().await;
    let mut c = client.connect(addr).await.unwrap();
    c.write_all(b"hello").await.unwrap();
    let mut s = listener.accept().await.unwrap();
    let mut buf = [0u8; 5];
    s.read_exact(&mut buf).await.unwrap();
    s.reset();
    let err = timeout(Duration::from_secs(5), c.read(&mut buf))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::ConnectionReset);
    assert_eq!(client.stats().resets, 1);
}

/// The DHT's side of a shared port: one UDP socket, KRPC handled here,
/// uTP handed to the socket.
async fn shared_port() -> (UtpSocket, SocketAddr, Arc<Mutex<Vec<Vec<u8>>>>) {
    // As the DHT holds it: a std half for sending, a tokio half receiving.
    let std = std::net::UdpSocket::bind(localhost()).unwrap();
    std.set_nonblocking(true).unwrap();
    let utp = UtpSocket::with_sink(Arc::new(std.try_clone().unwrap()), Config::default());
    let udp = UdpSocket::from_std(std).unwrap();
    let addr = udp.local_addr().unwrap();
    let krpc = Arc::new(Mutex::new(Vec::new()));
    let (u, k) = (utp.clone(), krpc.clone());
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65_536];
        while let Ok((n, from)) = udp.recv_from(&mut buf).await {
            match classify(&buf[..n]) {
                Protocol::Utp => {
                    u.feed(&buf[..n], from);
                }
                Protocol::Krpc => k.lock().unwrap().push(buf[..n].to_vec()),
                Protocol::Unknown => {}
            }
        }
    });
    (utp, addr, krpc)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shares_a_port_with_krpc() {
    let (utp, addr, krpc) = shared_port().await;
    let mut listener = utp.listen(4);
    tokio::spawn(async move {
        let mut s = listener.accept().await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        s.write_all(b"pong").await.unwrap();
        s.shutdown().await.unwrap();
    });
    // A KRPC query and a uTP connection to the same port.
    let other = UdpSocket::bind(localhost()).await.unwrap();
    other.send_to(b"d1:y1:qe", addr).await.unwrap();
    let client = bind_local().await;
    let mut c = client.connect(addr).await.unwrap();
    c.write_all(b"ping").await.unwrap();
    let mut got = Vec::new();
    c.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, b"pong");
    assert!(eventually(2, || krpc.lock().unwrap().len() == 1).await);
    assert_eq!(krpc.lock().unwrap()[0], b"d1:y1:qe");
    assert_eq!(utp.stats().invalid, 0, "no KRPC reached uTP");
}

/// Drops and delays outgoing packets: a lossy, reordering link.
struct LossySink {
    udp: Arc<std::net::UdpSocket>,
    rng: Mutex<StdRng>,
    loss: f64,
}

impl PacketSink for LossySink {
    fn send_to(&self, packet: &[u8], to: SocketAddr) -> std::io::Result<()> {
        let (drop_it, delay) = {
            let mut rng = self.rng.lock().unwrap();
            (rng.random_bool(self.loss), rng.random_range(0..20u64))
        };
        if drop_it {
            return Ok(());
        }
        let (udp, packet) = (self.udp.clone(), packet.to_vec());
        tokio::spawn(async move {
            sleep(Duration::from_millis(delay)).await;
            let _ = udp.send_to(&packet, to);
        });
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn survives_a_lossy_reordering_link() {
    let server = bind_local().await;
    let mut listener = server.listen(1);
    let addr = server.local_addr().unwrap();
    let payload = data(200_000);
    let expect = payload.clone();
    let srv = tokio::spawn(async move {
        let mut s = listener.accept().await.unwrap();
        let mut got = Vec::new();
        s.read_to_end(&mut got).await.unwrap();
        got
    });
    // The client's packets are lost 10% of the time and reordered; so are
    // the server's on the way in.
    let std = std::net::UdpSocket::bind(localhost()).unwrap();
    std.set_nonblocking(true).unwrap();
    let sink = Arc::new(LossySink {
        udp: Arc::new(std.try_clone().unwrap()),
        rng: Mutex::new(StdRng::seed_from_u64(29)),
        loss: 0.1,
    });
    let client = UtpSocket::with_sink(sink, Config::default());
    let udp = UdpSocket::from_std(std).unwrap();
    let feeder = client.clone();
    tokio::spawn(async move {
        let mut rng = StdRng::seed_from_u64(30);
        let mut buf = vec![0u8; 65_536];
        while let Ok((n, from)) = udp.recv_from(&mut buf).await {
            if !rng.random_bool(0.1) {
                feeder.feed(&buf[..n], from);
            }
        }
    });
    let mut c = timeout(Duration::from_secs(10), client.connect(addr))
        .await
        .unwrap()
        .unwrap();
    c.write_all(&payload).await.unwrap();
    c.shutdown().await.unwrap();
    let got = timeout(Duration::from_secs(60), srv)
        .await
        .unwrap()
        .unwrap();
    assert!(got == expect, "{} of {} bytes", got.len(), expect.len());
    assert!(
        client.stats().retransmits > 0,
        "loss forced retransmissions"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dual_stack_socket_reaches_ipv4_peers() {
    // Skipped where the host has no IPv6.
    let Ok(client) = UtpSocket::bind("[::]:0".parse().unwrap()).await else {
        return;
    };
    let server = bind_local().await;
    let mut listener = server.listen(1);
    let addr = server.local_addr().unwrap();
    tokio::spawn(async move {
        let mut s = listener.accept().await.unwrap();
        s.write_all(b"v4").await.unwrap();
        s.shutdown().await.unwrap();
    });
    let mut c = timeout(Duration::from_secs(5), client.connect(addr))
        .await
        .unwrap()
        .unwrap();
    let mut got = Vec::new();
    c.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, b"v4");
}

/// Records where each datagram went.
#[derive(Default)]
struct Recorder(Mutex<Vec<SocketAddr>>);

impl PacketSink for Recorder {
    fn send_to(&self, _: &[u8], to: SocketAddr) -> std::io::Result<()> {
        self.0.lock().unwrap().push(to);
        Ok(())
    }
}

#[tokio::test]
async fn dual_sink_routes_by_family() {
    let (v4, v6) = (Arc::new(Recorder::default()), Arc::new(Recorder::default()));
    let sink = lodestar_utp::DualSink {
        v4: Some(v4.clone()),
        v6: Some(v6.clone()),
    };
    let a: SocketAddr = "10.0.0.1:6881".parse().unwrap();
    let b: SocketAddr = "[2001:db8::1]:6881".parse().unwrap();
    sink.send_to(b"x", a).unwrap();
    sink.send_to(b"x", b).unwrap();
    assert_eq!(*v4.0.lock().unwrap(), [a]);
    assert_eq!(*v6.0.lock().unwrap(), [b]);
    let only_v4 = lodestar_utp::DualSink {
        v4: Some(v4),
        v6: None,
    };
    assert!(only_v4.send_to(b"x", b).is_err());
}

#[tokio::test]
async fn per_ip_cap_is_enforced() {
    // Default config: 8 connections per IP, pending ones included.
    let client = UtpSocket::bind(localhost()).await.unwrap();
    let quiet = bind_local().await;
    let target = quiet.local_addr().unwrap();
    let mut pending = Vec::new();
    for _ in 0..8 {
        let c = client.clone();
        pending.push(tokio::spawn(
            async move { c.connect(target).await.map(drop) },
        ));
    }
    assert!(eventually(2, || client.connections() == 8).await);
    let err = client.connect(target).await.unwrap_err();
    assert!(err.to_string().contains("too many"), "{err}");
    // Another IP is unaffected (only the cap is checked, not reachability).
    let other: SocketAddr = "127.0.0.2:9".parse().unwrap();
    let c = client.clone();
    let ninth = tokio::spawn(async move { c.connect(other).await.map(drop) });
    assert!(eventually(2, || client.connections() == 9).await);
    // Abandoned attempts free their slots.
    for p in pending {
        p.abort();
    }
    ninth.abort();
    assert!(eventually(2, || client.connections() == 0).await);
    let c = client.clone();
    let again = tokio::spawn(async move { c.connect(target).await.map(drop) });
    assert!(eventually(2, || client.connections() == 1).await);
    again.abort();
}
