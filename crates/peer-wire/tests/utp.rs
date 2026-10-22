//! Metadata fetches over uTP (BEP 29) against the fake peer on loopback,
//! including a lossy, reordering link and the fallback to TCP.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer, Seed};
use lodestar_peer_wire::utp::{Config, PacketSink, UtpSocket};
use lodestar_peer_wire::{Encryption, Error, FetchOptions, FetchStats, Transport, fetch};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

/// A realistic info dict of roughly `size` bytes (grown via file entries).
fn info_dict(size: usize) -> Vec<u8> {
    let mut files = Vec::new();
    let mut i = 0;
    while to_vec(&OwnedValue::List(files.clone())).len() < size {
        let mut f = std::collections::BTreeMap::new();
        f.insert(b"length".to_vec(), OwnedValue::Int(1000 + i));
        f.insert(
            b"path".to_vec(),
            OwnedValue::List(vec![OwnedValue::from(
                format!("Season 1/Episode {i:05}.mkv").as_str(),
            )]),
        );
        files.push(OwnedValue::Dict(f));
        i += 1;
    }
    let mut d = std::collections::BTreeMap::new();
    d.insert(b"files".to_vec(), OwnedValue::List(files));
    d.insert(b"name".to_vec(), OwnedValue::from("Test Pack"));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 20));
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20]));
    to_vec(&OwnedValue::Dict(d))
}

async fn client() -> UtpSocket {
    // Every fake peer is on 127.0.0.1: lift the per-IP cap.
    UtpSocket::bind_with(
        "127.0.0.1:0".parse().unwrap(),
        Config {
            max_per_ip: 1024,
            ..Config::default()
        },
    )
    .await
    .unwrap()
}

fn utp_opts(utp: UtpSocket, encryption: Encryption) -> FetchOptions {
    FetchOptions {
        timeout: Duration::from_secs(5),
        encryption,
        transport: Transport::Utp,
        fallback_to_tcp: false,
        utp: Some(utp),
        ..FetchOptions::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_future_is_send_with_utp() {
    fn assert_send<T: Send>(_: &T) {}
    let o = utp_opts(client().await, Encryption::Prefer);
    let f = fetch("127.0.0.1:1".parse().unwrap(), InfoHash([0; 20]), &o);
    assert_send(&f);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetches_plaintext_and_encrypted_over_utp() {
    let info = info_dict(40_000); // three metadata pieces
    let seed = Seed::v1(info.clone());
    let hash = InfoHash(seed.info_hash);
    let utp = client().await;
    let cases = [
        (Accept::Both, Encryption::Prefer, true),
        (Accept::Both, Encryption::Disable, false),
        (Accept::Both, Encryption::Require, true),
        (Accept::EncryptedOnly, Encryption::Prefer, true),
        (Accept::EncryptedOnly, Encryption::Require, true),
        (Accept::PlaintextOnly, Encryption::Prefer, false), // falls back
        (Accept::PlaintextOnly, Encryption::Disable, false),
    ];
    let opts = |mode| utp_opts(utp.clone(), mode);
    let stats = opts(Encryption::Prefer).stats;
    for (accept, mode, encrypted) in cases {
        let peer = FakePeer::spawn_utp(vec![seed.clone()], accept, Behavior::Honest, vec![]).await;
        let o = FetchOptions {
            stats: stats.clone(),
            ..opts(mode)
        };
        let got = fetch(peer.addr(), hash, &o)
            .await
            .unwrap_or_else(|e| panic!("{accept:?}/{mode:?}: {e}"));
        assert_eq!(got.info, info, "{accept:?}/{mode:?}");
        assert_eq!(got.encrypted, encrypted, "{accept:?}/{mode:?}");
        assert_eq!(got.client.as_deref(), Some("FakePeer 1.0"));
        assert_eq!(Info::parse(&got.info).unwrap().primary_hash(), hash);
    }
    let n = cases.len() as u64;
    assert_eq!(stats.attempts(Transport::Utp), n);
    assert_eq!(stats.successes(Transport::Utp), n);
    assert_eq!(stats.attempts(Transport::Tcp), 0, "never touched TCP");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_peers_fail_the_same_way_over_utp() {
    let seed = Seed::v1(info_dict(20_000));
    let hash = InfoHash(seed.info_hash);
    let utp = client().await;
    let run = |behavior| {
        let (seed, utp) = (seed.clone(), utp.clone());
        async move {
            let peer = FakePeer::spawn_utp(vec![seed], Accept::Both, behavior, vec![]).await;
            let o = FetchOptions {
                timeout: Duration::from_secs(3),
                ..utp_opts(utp, Encryption::Prefer)
            };
            fetch(peer.addr(), hash, &o).await
        }
    };
    assert!(matches!(
        run(Behavior::WrongData).await,
        Err(Error::HashMismatch)
    ));
    assert!(matches!(run(Behavior::Reject).await, Err(Error::Rejected)));
    assert!(matches!(
        run(Behavior::NoExtensions).await,
        Err(Error::NoExtensions)
    ));
    assert!(matches!(
        run(Behavior::LieAboutSize(1 << 40)).await,
        Err(Error::MetadataSize(_))
    ));
    assert!(matches!(
        run(Behavior::LieAboutSize(1000)).await,
        Err(Error::Protocol(_))
    ));
    let started = std::time::Instant::now();
    assert!(matches!(run(Behavior::Stall).await, Err(Error::Timeout)));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the same deadline applies"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn falls_back_to_tcp_when_utp_goes_unanswered() {
    let info = info_dict(500);
    let seed = Seed::v1(info.clone());
    let hash = InfoHash(seed.info_hash);
    // A TCP-only peer: nothing answers uTP on its port.
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let o = FetchOptions {
        fallback_to_tcp: true,
        utp_fallback_after: Duration::from_millis(300),
        ..utp_opts(client().await, Encryption::Prefer)
    };
    let got = fetch(peer.addr(), hash, &o).await.unwrap();
    assert_eq!(got.info, info);
    let s = &o.stats;
    assert_eq!(s.attempts(Transport::Utp), 1);
    assert_eq!(s.successes(Transport::Utp), 0);
    assert_eq!(s.connect_timeouts(Transport::Utp), 1);
    assert_eq!(s.attempts(Transport::Tcp), 1);
    assert_eq!(s.successes(Transport::Tcp), 1);

    // Without the fallback the fetch fails as a timeout.
    let o = FetchOptions {
        fallback_to_tcp: false,
        connect_timeout: Duration::from_millis(300),
        ..o
    };
    assert!(matches!(
        fetch(peer.addr(), hash, &o).await,
        Err(Error::Timeout)
    ));

    // uTP asked for but no socket: TCP when allowed, an error otherwise.
    let o = FetchOptions {
        utp: None,
        fallback_to_tcp: true,
        stats: Arc::new(FetchStats::default()),
        ..o
    };
    fetch(peer.addr(), hash, &o).await.unwrap();
    assert_eq!(o.stats.attempts(Transport::Utp), 0);
    let o = FetchOptions {
        fallback_to_tcp: false,
        ..o
    };
    assert!(matches!(
        fetch(peer.addr(), hash, &o).await,
        Err(Error::Connect(_))
    ));
}

#[tokio::test]
async fn default_options_stay_on_tcp() {
    let seed = Seed::v1(info_dict(500));
    let hash = InfoHash(seed.info_hash);
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let o = FetchOptions::default();
    assert_eq!(o.transport, Transport::Tcp);
    assert!(o.utp.is_none());
    fetch(peer.addr(), hash, &o).await.unwrap();
    assert_eq!(o.stats.attempts(Transport::Tcp), 1);
    assert_eq!(o.stats.successes(Transport::Tcp), 1);
    assert_eq!(o.stats.attempts(Transport::Utp), 0);
}

/// Drops 15% of outgoing packets and delays the rest by up to 30 ms, so
/// they also arrive out of order.
struct LossySink {
    udp: Arc<std::net::UdpSocket>,
    rng: Mutex<StdRng>,
}

impl PacketSink for LossySink {
    fn send_to(&self, packet: &[u8], to: SocketAddr) -> std::io::Result<()> {
        let (lost, delay) = {
            let mut rng = self.rng.lock().unwrap();
            (rng.random_bool(0.15), rng.random_range(0..30u64))
        };
        if !lost {
            let (udp, packet) = (self.udp.clone(), packet.to_vec());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                let _ = udp.send_to(&packet, to);
            });
        }
        Ok(())
    }
}

/// A client socket on a link that loses and reorders in both directions.
fn lossy_client(seed: u64) -> UtpSocket {
    let std = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    std.set_nonblocking(true).unwrap();
    let sink = Arc::new(LossySink {
        udp: Arc::new(std.try_clone().unwrap()),
        rng: Mutex::new(StdRng::seed_from_u64(seed)),
    });
    let utp = UtpSocket::with_sink(
        sink,
        Config {
            max_per_ip: 1024,
            ..Config::default()
        },
    );
    let udp = tokio::net::UdpSocket::from_std(std).unwrap();
    let feeder = utp.clone();
    tokio::spawn(async move {
        let mut rng = StdRng::seed_from_u64(seed ^ 0xff);
        let mut buf = vec![0u8; 65_536];
        while let Ok((n, from)) = udp.recv_from(&mut buf).await {
            if !rng.random_bool(0.15) {
                feeder.feed(&buf[..n], from);
            }
        }
    });
    utp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetches_over_a_lossy_link() {
    let info = info_dict(120_000); // eight metadata pieces
    let seed = Seed::v1(info.clone());
    let hash = InfoHash(seed.info_hash);
    for (i, mode) in [Encryption::Require, Encryption::Disable]
        .into_iter()
        .enumerate()
    {
        let utp = lossy_client(40 + i as u64);
        let peer =
            FakePeer::spawn_utp(vec![seed.clone()], Accept::Both, Behavior::Honest, vec![]).await;
        let o = FetchOptions {
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            ..utp_opts(utp.clone(), mode)
        };
        let got = fetch(peer.addr(), hash, &o)
            .await
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert_eq!(got.info, info, "{mode:?}");
        assert_eq!(got.encrypted, mode == Encryption::Require);
        let resent = utp.stats().retransmits + peer.utp().map_or(0, |p| p.stats().retransmits);
        assert!(resent > 0, "the link really lost packets");
    }
}
