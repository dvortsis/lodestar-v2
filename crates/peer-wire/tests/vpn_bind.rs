//! X-vpn-bind: a fetch with a guard dials only through its interface,
//! and not at all while the guard says it's down. Linux: it's where an
//! unprivileged test can name an interface that exists (`lo`) and one
//! that doesn't.
#![cfg(target_os = "linux")]

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::InfoHash;
use lodestar_network_policy::iface::{Down, Guard, Interface, Link, State};
use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer, Seed};
use lodestar_peer_wire::{Error, FetchOptions, fetch};

fn opts(guard: std::sync::Arc<Guard>) -> FetchOptions {
    FetchOptions {
        allow_private: true,
        timeout: Duration::from_secs(10),
        connect_timeout: Duration::from_secs(10),
        guard: Some(guard),
        ..FetchOptions::default()
    }
}

fn seed() -> Seed {
    let mut d = std::collections::BTreeMap::new();
    d.insert(b"name".to_vec(), OwnedValue::from("Test"));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 18));
    d.insert(b"length".to_vec(), OwnedValue::Int(1));
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20]));
    Seed::v1(to_vec(&OwnedValue::Dict(d)))
}

/// A listener that records whether anything connected: a connect that
/// succeeded is in its backlog by the time `connect` returns.
fn listener() -> (std::net::TcpListener, SocketAddr) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let addr = l.local_addr().unwrap();
    (l, addr)
}

fn nothing_connected(l: &std::net::TcpListener) -> bool {
    matches!(l.accept(), Err(e) if e.kind() == ErrorKind::WouldBlock)
}

#[tokio::test]
async fn pinned_to_loopback_a_fetch_still_works() {
    let seed = seed();
    let (hash, seed_info) = (InfoHash(seed.info_hash), seed.info.clone());
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let guard = Guard::new(Interface::parse("lo").unwrap());
    assert!(guard.state().is_up());
    let got = fetch(peer.addr(), hash, &opts(guard)).await.unwrap();
    assert_eq!(got.info, seed_info);
}

#[tokio::test]
async fn a_down_guard_refuses_before_any_packet() {
    let (l, addr) = listener();
    let guard = Guard::with_probe(Interface::parse("lo").unwrap(), |_| Err(Down::Missing));
    let r = fetch(addr, InfoHash([1; 20]), &opts(guard)).await;
    assert!(
        matches!(&r, Err(Error::Connect(e)) if e.kind() == ErrorKind::NetworkDown),
        "{r:?}"
    );
    assert!(nothing_connected(&l));
}

/// The socket is the safety, not the probe: a probe that wrongly says the
/// interface is up still can't make a fetch leave by the normal route.
/// The failure makes the guard check at once; the (lying) check decides.
#[tokio::test]
async fn a_probe_that_lies_still_cant_leak() {
    let (l, addr) = listener();
    let guard = Guard::with_probe(
        Interface::parse("lsnone0").unwrap(),
        |_| Ok(Link::default()),
    );
    assert!(guard.state().is_up(), "the probe lies");
    let r = fetch(addr, InfoHash([1; 20]), &opts(guard.clone())).await;
    assert!(
        matches!(&r, Err(Error::Connect(e)) if lodestar_network_policy::iface::pin_failure(e).and_then(std::io::Error::raw_os_error) == Some(19)),
        "ENODEV, not a connection: {r:?}"
    );
    assert!(nothing_connected(&l));
    // ENODEV trips the guard at once; the (lying) check brings it back.
    assert_eq!(guard.state(), State::Down(Down::Down));
    assert!(guard.poll().is_up(), "the check said up, so up");
}
