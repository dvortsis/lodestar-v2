//! End-to-end metadata fetches against the fake peer over real TCP.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer, Seed};
use lodestar_peer_wire::{Deny, Encryption, Error, FetchOptions, fetch};
use sha2::{Digest, Sha256};

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

fn opts(encryption: Encryption) -> FetchOptions {
    FetchOptions {
        timeout: Duration::from_secs(3),
        encryption,
        ..FetchOptions::default()
    }
}

/// The pipeline spawns fetches onto a multi-threaded runtime.
#[test]
fn fetch_future_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let o = opts(Encryption::Prefer);
    let f = fetch("127.0.0.1:1".parse().unwrap(), InfoHash([0; 20]), &o);
    assert_send(&f);
}

#[tokio::test]
async fn fetches_across_encryption_combinations() {
    let info = info_dict(40_000); // three metadata pieces
    let seed = Seed::v1(info.clone());
    let hash = InfoHash(seed.info_hash);
    let cases = [
        (Accept::Both, Encryption::Prefer, true),
        (Accept::Both, Encryption::Disable, false),
        (Accept::Both, Encryption::Require, true),
        (Accept::EncryptedOnly, Encryption::Prefer, true),
        (Accept::EncryptedOnly, Encryption::Require, true),
        (Accept::PlaintextOnly, Encryption::Prefer, false), // falls back
        (Accept::PlaintextOnly, Encryption::Disable, false),
    ];
    for (accept, mode, encrypted) in cases {
        let peer = FakePeer::spawn(vec![seed.clone()], accept, Behavior::Honest, vec![]).await;
        let got = fetch(peer.addr(), hash, &opts(mode))
            .await
            .unwrap_or_else(|e| panic!("{accept:?}/{mode:?}: {e}"));
        assert_eq!(got.info, info, "{accept:?}/{mode:?}");
        assert!(!got.v2);
        assert_eq!(got.encrypted, encrypted, "{accept:?}/{mode:?}");
        assert_eq!(got.client.as_deref(), Some("FakePeer 1.0"));
        // The fetched dict is real metainfo.
        assert_eq!(Info::parse(&got.info).unwrap().primary_hash(), hash);
    }
}

#[tokio::test]
async fn impossible_combinations_fail_cleanly() {
    let seed = Seed::v1(info_dict(100));
    let hash = InfoHash(seed.info_hash);
    let peer = FakePeer::spawn(
        vec![seed.clone()],
        Accept::EncryptedOnly,
        Behavior::Honest,
        vec![],
    )
    .await;
    assert!(
        fetch(peer.addr(), hash, &opts(Encryption::Disable))
            .await
            .is_err()
    );
    let peer = FakePeer::spawn(vec![seed], Accept::PlaintextOnly, Behavior::Honest, vec![]).await;
    assert!(matches!(
        fetch(peer.addr(), hash, &opts(Encryption::Require)).await,
        Err(Error::Mse(_))
    ));
}

#[tokio::test]
async fn verifies_v2_keys_by_truncated_sha256() {
    let info = info_dict(500);
    let key: [u8; 20] = Sha256::digest(&info)[..20].try_into().unwrap();
    let seed = Seed {
        info: info.clone(),
        info_hash: key,
    };
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let got = fetch(peer.addr(), InfoHash(key), &opts(Encryption::Prefer))
        .await
        .unwrap();
    assert!(got.v2);
    assert_eq!(got.info, info);
}

#[tokio::test]
async fn collects_pex_peers() {
    let seed = Seed::v1(info_dict(100));
    let hash = InfoHash(seed.info_hash);
    let pex: Vec<SocketAddr> = vec![
        "10.1.2.3:6881".parse().unwrap(),
        "[2001:db8::7]:51413".parse().unwrap(),
    ];
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, pex.clone()).await;
    let got = fetch(peer.addr(), hash, &opts(Encryption::Prefer))
        .await
        .unwrap();
    assert_eq!(got.pex, pex);
}

#[tokio::test]
async fn hostile_and_broken_peers() {
    let seed = Seed::v1(info_dict(20_000));
    let hash = InfoHash(seed.info_hash);
    let run = |behavior| {
        let seed = seed.clone();
        async move {
            let peer = FakePeer::spawn(vec![seed], Accept::Both, behavior, vec![]).await;
            fetch(peer.addr(), hash, &opts(Encryption::Prefer)).await
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
        run(Behavior::LieAboutSize(0)).await,
        Err(Error::MetadataSize(0))
    ));
    assert!(matches!(
        run(Behavior::LieAboutSize(1 << 40)).await,
        Err(Error::MetadataSize(_))
    ));
    // Claiming a smaller size makes the real pieces the wrong length.
    assert!(matches!(
        run(Behavior::LieAboutSize(1000)).await,
        Err(Error::Protocol(_))
    ));
    let started = std::time::Instant::now();
    assert!(matches!(run(Behavior::Stall).await, Err(Error::Timeout)));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "deadline enforced"
    );
}

#[tokio::test]
async fn unknown_hash_and_unreachable_peer() {
    let seed = Seed::v1(info_dict(100));
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    // A hash the peer doesn't have: it hangs up.
    assert!(
        fetch(peer.addr(), InfoHash([9; 20]), &opts(Encryption::Disable))
            .await
            .is_err()
    );
    // Nobody listening.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    assert!(matches!(
        fetch(closed, InfoHash([9; 20]), &opts(Encryption::Disable)).await,
        Err(Error::Connect(_))
    ));
}

#[tokio::test]
async fn denied_addresses_are_never_connected_to() {
    let seed = Seed::v1(info_dict(100));
    let hash = InfoHash(seed.info_hash);
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let mut o = opts(Encryption::Prefer);
    o.deny = Some(Deny(Arc::new(|ip| ip.is_loopback())));
    let err = fetch(peer.addr(), hash, &o).await.unwrap_err();
    assert!(matches!(err, Error::Denied), "{err}");
    o.deny = Some(Deny(Arc::new(|_| false)));
    assert!(fetch(peer.addr(), hash, &o).await.is_ok());
}
